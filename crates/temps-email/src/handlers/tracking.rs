//! Email tracking handlers for open tracking (pixel) and click tracking (redirect)

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{
    error_builder::{bad_request, internal_server_error, not_found},
    problemdetails::Problem,
    RequestMetadata,
};
use tracing::{error, warn};
use utoipa::ToSchema;
use uuid::Uuid;

use super::types::AppState;

/// Extract IP and user agent from RequestMetadata extension (if available) or headers
fn extract_metadata(
    metadata: &Option<axum::Extension<RequestMetadata>>,
    headers: &axum::http::HeaderMap,
) -> (Option<String>, Option<String>) {
    if let Some(axum::Extension(meta)) = metadata {
        (Some(meta.ip_address.clone()), Some(meta.user_agent.clone()))
    } else {
        let ip = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').next())
            .map(|s| s.trim().to_string());
        let ua = headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        (ip, ua)
    }
}

// 1x1 transparent GIF
const TRACKING_PIXEL: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xff, 0xff, 0xff,
    0x00, 0x00, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
];

/// Configure tracking routes (public, no auth required)
pub fn public_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/emails/{email_id}/track/open", get(track_open))
        .route(
            "/emails/{email_id}/track/click/{link_index}",
            get(track_click),
        )
}

/// Configure authenticated tracking data routes
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/emails/events/stats", get(get_global_event_stats))
        .route("/emails/events", get(get_global_events))
        .route("/emails/{id}/tracking", get(get_email_tracking))
        .route("/emails/{id}/tracking/events", get(get_email_events))
        .route("/emails/{id}/tracking/links", get(get_email_links))
}

/// Track email open - returns a 1x1 transparent GIF
///
/// This endpoint is embedded as an <img> tag in emails.
/// No authentication required - it's called by the email client.
#[utoipa::path(
    tag = "Email Tracking",
    get,
    path = "/emails/{email_id}/track/open",
    responses(
        (status = 200, description = "1x1 transparent tracking pixel"),
        (status = 404, description = "Email not found")
    ),
    params(
        ("email_id" = String, Path, description = "Email ID (UUID)")
    )
)]
pub async fn track_open(
    State(state): State<Arc<AppState>>,
    Path(email_id): Path<String>,
    metadata: Option<axum::Extension<RequestMetadata>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let email_id = match Uuid::parse_str(&email_id) {
        Ok(id) => id,
        Err(_) => {
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "image/gif")],
                TRACKING_PIXEL.to_vec(),
            )
                .into_response();
        }
    };

    let (ip, ua) = extract_metadata(&metadata, &headers);

    if let Err(e) = state.tracking_service.record_open(email_id, ip, ua).await {
        warn!("Failed to record open event for email {}: {}", email_id, e);
    }

    // Always return the pixel, even if recording failed
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/gif"),
            (header::CACHE_CONTROL, "no-store, no-cache, must-revalidate"),
        ],
        TRACKING_PIXEL.to_vec(),
    )
        .into_response()
}

/// Track email link click - redirects to original URL
///
/// This endpoint replaces original links in tracked emails.
/// No authentication required - it's called when the recipient clicks a link.
#[utoipa::path(
    tag = "Email Tracking",
    get,
    path = "/emails/{email_id}/track/click/{link_index}",
    responses(
        (status = 302, description = "Redirect to original URL"),
        (status = 404, description = "Link not found")
    ),
    params(
        ("email_id" = String, Path, description = "Email ID (UUID)"),
        ("link_index" = i32, Path, description = "Link index")
    )
)]
pub async fn track_click(
    State(state): State<Arc<AppState>>,
    Path((email_id, link_index)): Path<(String, i32)>,
    metadata: Option<axum::Extension<RequestMetadata>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let email_id = match Uuid::parse_str(&email_id) {
        Ok(id) => id,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "Invalid email ID").into_response();
        }
    };

    let (ip, ua) = extract_metadata(&metadata, &headers);

    match state
        .tracking_service
        .record_click(email_id, link_index, ip, ua)
        .await
    {
        Ok(redirect_url) => Redirect::temporary(&redirect_url).into_response(),
        Err(e) => {
            warn!(
                "Failed to record click for email {} link {}: {}",
                email_id, link_index, e
            );
            (StatusCode::NOT_FOUND, "Link not found").into_response()
        }
    }
}

// ============================================================
// GLOBAL TRACKING ENDPOINTS
// ============================================================

#[derive(Debug, Serialize, ToSchema)]
pub struct GlobalEventStatsResponse {
    pub delivered: u64,
    pub opened: u64,
    pub clicked: u64,
    pub bounced: u64,
    pub complained: u64,
    pub open_rate: Option<f64>,
    pub click_rate: Option<f64>,
    pub bounce_rate: Option<f64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PaginatedEventsResponse {
    pub events: Vec<TrackingEventResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Deserialize)]
pub struct GlobalEventsQuery {
    pub event_type: Option<String>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

/// GET /emails/events/stats
#[utoipa::path(
    tag = "Email Tracking",
    get,
    path = "/emails/events/stats",
    responses(
        (status = 200, description = "Global tracking statistics", body = GlobalEventStatsResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_global_event_stats(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsRead);

    let stats = state
        .tracking_service
        .get_global_stats()
        .await
        .map_err(|e| {
            error!("Failed to get global tracking stats: {}", e);
            internal_server_error()
                .detail("Failed to get tracking statistics")
                .build()
        })?;

    Ok(Json(GlobalEventStatsResponse {
        delivered: stats.delivered,
        opened: stats.opened,
        clicked: stats.clicked,
        bounced: stats.bounced,
        complained: stats.complained,
        open_rate: stats.open_rate,
        click_rate: stats.click_rate,
        bounce_rate: stats.bounce_rate,
    }))
}

/// GET /emails/events
#[utoipa::path(
    tag = "Email Tracking",
    get,
    path = "/emails/events",
    params(
        ("event_type" = Option<String>, Query, description = "Filter by event type (open, click)"),
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<u64>, Query, description = "Page size (default: 20, max: 100)"),
    ),
    responses(
        (status = 200, description = "Paginated tracking events", body = PaginatedEventsResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_global_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<GlobalEventsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsRead);

    let page = query.page.unwrap_or(1);
    let page_size = std::cmp::min(query.page_size.unwrap_or(20), 100);

    let (events, total) = state
        .tracking_service
        .get_all_events(query.event_type.as_deref(), page, page_size)
        .await
        .map_err(|e| {
            error!("Failed to get global events: {}", e);
            internal_server_error()
                .detail("Failed to get tracking events")
                .build()
        })?;

    let response = PaginatedEventsResponse {
        events: events
            .into_iter()
            .map(|e| TrackingEventResponse {
                id: e.id,
                email_id: e.email_id.to_string(),
                event_type: e.event_type,
                link_url: e.link_url,
                link_index: e.link_index,
                ip_address: e.ip_address,
                user_agent: e.user_agent,
                created_at: e.created_at.to_rfc3339(),
            })
            .collect(),
        total,
        page,
        page_size,
    };

    Ok(Json(response))
}

// ============================================================
// PER-EMAIL TRACKING ENDPOINTS
// ============================================================

/// Email tracking summary
#[derive(Debug, Serialize, ToSchema)]
pub struct EmailTrackingResponse {
    pub email_id: String,
    pub track_opens: bool,
    pub track_clicks: bool,
    pub open_count: i32,
    pub click_count: i32,
    pub first_opened_at: Option<String>,
    pub first_clicked_at: Option<String>,
    pub unique_opens: u64,
    pub unique_clicks: u64,
    pub links: Vec<TrackedLinkResponse>,
}

/// Tracked link with click count
#[derive(Debug, Serialize, ToSchema)]
pub struct TrackedLinkResponse {
    pub link_index: i32,
    pub original_url: String,
    pub click_count: i32,
}

/// Email tracking event
#[derive(Debug, Serialize, ToSchema)]
pub struct TrackingEventResponse {
    pub id: i64,
    pub email_id: String,
    pub event_type: String,
    pub link_url: Option<String>,
    pub link_index: Option<i32>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: String,
}

/// Get email tracking summary
#[utoipa::path(
    tag = "Email Tracking",
    get,
    path = "/emails/{id}/tracking",
    responses(
        (status = 200, description = "Tracking summary", body = EmailTrackingResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Email not found")
    ),
    params(
        ("id" = String, Path, description = "Email ID (UUID)")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_email_tracking(
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

    let links = state
        .tracking_service
        .get_links(email_id)
        .await
        .map_err(|e| {
            error!("Failed to get tracking links: {}", e);
            internal_server_error()
                .detail("Failed to get tracking data")
                .build()
        })?;

    let events = state
        .tracking_service
        .get_events(email_id, None)
        .await
        .map_err(|e| {
            error!("Failed to get tracking events: {}", e);
            internal_server_error()
                .detail("Failed to get tracking data")
                .build()
        })?;

    // Count unique IPs for opens/clicks
    let unique_opens = events
        .iter()
        .filter(|e| e.event_type == "opened")
        .filter_map(|e| e.ip_address.as_ref())
        .collect::<std::collections::HashSet<_>>()
        .len() as u64;

    let unique_clicks = events
        .iter()
        .filter(|e| e.event_type == "clicked")
        .filter_map(|e| e.ip_address.as_ref())
        .collect::<std::collections::HashSet<_>>()
        .len() as u64;

    let response = EmailTrackingResponse {
        email_id: email.id.to_string(),
        track_opens: email.track_opens,
        track_clicks: email.track_clicks,
        open_count: email.open_count,
        click_count: email.click_count,
        first_opened_at: email.first_opened_at.map(|dt| dt.to_rfc3339()),
        first_clicked_at: email.first_clicked_at.map(|dt| dt.to_rfc3339()),
        unique_opens,
        unique_clicks,
        links: links
            .into_iter()
            .map(|l| TrackedLinkResponse {
                link_index: l.link_index,
                original_url: l.original_url,
                click_count: l.click_count,
            })
            .collect(),
    };

    Ok(Json(response))
}

/// Get email tracking events
#[utoipa::path(
    tag = "Email Tracking",
    get,
    path = "/emails/{id}/tracking/events",
    responses(
        (status = 200, description = "Tracking events", body = Vec<TrackingEventResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Email not found")
    ),
    params(
        ("id" = String, Path, description = "Email ID (UUID)"),
        ("event_type" = Option<String>, Query, description = "Filter by event type (open, click)")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_email_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsRead);

    let email_id = Uuid::parse_str(&id)
        .map_err(|_| bad_request().detail("Invalid email ID format").build())?;

    let events = state
        .tracking_service
        .get_events(email_id, query.event_type.as_deref())
        .await
        .map_err(|e| {
            error!("Failed to get tracking events: {}", e);
            internal_server_error()
                .detail("Failed to get tracking events")
                .build()
        })?;

    let response: Vec<TrackingEventResponse> = events
        .into_iter()
        .map(|e| TrackingEventResponse {
            id: e.id,
            email_id: e.email_id.to_string(),
            event_type: e.event_type,
            link_url: e.link_url,
            link_index: e.link_index,
            ip_address: e.ip_address,
            user_agent: e.user_agent,
            created_at: e.created_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(response))
}

/// Get tracked links for an email
#[utoipa::path(
    tag = "Email Tracking",
    get,
    path = "/emails/{id}/tracking/links",
    responses(
        (status = 200, description = "Tracked links", body = Vec<TrackedLinkResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Email not found")
    ),
    params(
        ("id" = String, Path, description = "Email ID (UUID)")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_email_links(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EmailsRead);

    let email_id = Uuid::parse_str(&id)
        .map_err(|_| bad_request().detail("Invalid email ID format").build())?;

    let links = state
        .tracking_service
        .get_links(email_id)
        .await
        .map_err(|e| {
            error!("Failed to get tracking links: {}", e);
            internal_server_error()
                .detail("Failed to get tracking links")
                .build()
        })?;

    let response: Vec<TrackedLinkResponse> = links
        .into_iter()
        .map(|l| TrackedLinkResponse {
            link_index: l.link_index,
            original_url: l.original_url,
            click_count: l.click_count,
        })
        .collect();

    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    pub event_type: Option<String>,
}
