//! Analytics API Handlers
//!
//! Provides endpoints compatible with @temps-sdk/react-analytics SDK
//! and inspector endpoints for the UI.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::services::AnalyticsService;

/// Shared state for analytics handlers
pub struct AnalyticsState {
    pub analytics_service: Arc<AnalyticsService>,
}

// =============================================================================
// SDK Endpoints (what @temps-sdk/react-analytics calls)
// =============================================================================

/// Handle regular analytics events (page_view, page_leave, heartbeat, custom)
/// POST /api/_temps/event
pub async fn handle_event(
    State(state): State<Arc<AnalyticsState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    match state.analytics_service.ingest_event(payload).await {
        Ok(id) => (StatusCode::OK, Json(EventResponse { id, success: true })),
        Err(e) => {
            tracing::error!("Failed to ingest event: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(EventResponse {
                    id: 0,
                    success: false,
                }),
            )
        }
    }
}

/// Handle speed analytics (web vitals)
/// POST /api/_temps/speed
pub async fn handle_speed(
    State(state): State<Arc<AnalyticsState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    match state.analytics_service.ingest_speed(payload).await {
        Ok(id) => (StatusCode::OK, Json(EventResponse { id, success: true })),
        Err(e) => {
            tracing::error!("Failed to ingest speed metrics: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(EventResponse {
                    id: 0,
                    success: false,
                }),
            )
        }
    }
}

/// Handle session replay initialization
/// POST /api/_temps/session-replay/init
pub async fn handle_session_init(
    State(state): State<Arc<AnalyticsState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    match state.analytics_service.ingest_session_init(payload).await {
        Ok(_id) => StatusCode::CREATED,
        Err(e) => {
            tracing::error!("Failed to init session replay: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// Handle session replay events (rrweb data)
/// POST /api/_temps/session-replay/events
pub async fn handle_session_events(
    State(state): State<Arc<AnalyticsState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    match state.analytics_service.ingest_session_events(payload).await {
        Ok(id) => (StatusCode::OK, Json(EventResponse { id, success: true })),
        Err(e) => {
            tracing::error!("Failed to ingest session events: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(EventResponse {
                    id: 0,
                    success: false,
                }),
            )
        }
    }
}

// =============================================================================
// Inspector Endpoints (for UI to read data)
// =============================================================================

#[derive(Deserialize)]
pub struct ListEventsQuery {
    #[serde(default = "default_limit")]
    pub limit: u64,
    #[serde(default)]
    pub offset: u64,
    pub event_type: Option<String>,
    pub event_name: Option<String>,
}

fn default_limit() -> u64 {
    50
}

/// List captured events
/// GET /api/inspector/events
pub async fn list_events(
    State(state): State<Arc<AnalyticsState>>,
    Query(query): Query<ListEventsQuery>,
) -> impl IntoResponse {
    match state
        .analytics_service
        .list_events(
            query.limit,
            query.offset,
            query.event_type.as_deref(),
            query.event_name.as_deref(),
        )
        .await
    {
        Ok(events) => {
            // Parse payload JSON for each event
            let events_with_parsed: Vec<EventWithParsedPayload> = events
                .into_iter()
                .map(|e| {
                    let parsed_payload = serde_json::from_str(&e.payload).unwrap_or(Value::Null);
                    EventWithParsedPayload {
                        id: e.id,
                        event_type: e.event_type,
                        event_name: e.event_name,
                        request_path: e.request_path,
                        request_query: e.request_query,
                        domain: e.domain,
                        session_id: e.session_id,
                        request_id: e.request_id,
                        payload: parsed_payload,
                        received_at: e.received_at,
                    }
                })
                .collect();

            (StatusCode::OK, Json(events_with_parsed))
        }
        Err(e) => {
            tracing::error!("Failed to list events: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(vec![]))
        }
    }
}

/// Get a single event by ID
/// GET /api/inspector/events/:id
pub async fn get_event(
    State(state): State<Arc<AnalyticsState>>,
    Path(id): Path<i32>,
) -> impl IntoResponse {
    match state.analytics_service.get_event(id).await {
        Ok(Some(e)) => {
            let parsed_payload = serde_json::from_str(&e.payload).unwrap_or(Value::Null);
            let event = EventWithParsedPayload {
                id: e.id,
                event_type: e.event_type,
                event_name: e.event_name,
                request_path: e.request_path,
                request_query: e.request_query,
                domain: e.domain,
                session_id: e.session_id,
                request_id: e.request_id,
                payload: parsed_payload,
                received_at: e.received_at,
            };
            (StatusCode::OK, Json(Some(event)))
        }
        Ok(None) => (StatusCode::NOT_FOUND, Json(None)),
        Err(e) => {
            tracing::error!("Failed to get event: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(None))
        }
    }
}

/// Clear all events
/// DELETE /api/inspector/events
pub async fn clear_events(State(state): State<Arc<AnalyticsState>>) -> impl IntoResponse {
    match state.analytics_service.clear_events().await {
        Ok(count) => (
            StatusCode::OK,
            Json(ClearResponse {
                deleted: count,
                success: true,
            }),
        ),
        Err(e) => {
            tracing::error!("Failed to clear events: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ClearResponse {
                    deleted: 0,
                    success: false,
                }),
            )
        }
    }
}

/// Get event count
/// GET /api/inspector/events/count
pub async fn count_events(State(state): State<Arc<AnalyticsState>>) -> impl IntoResponse {
    match state.analytics_service.count_events().await {
        Ok(count) => (StatusCode::OK, Json(CountResponse { count })),
        Err(e) => {
            tracing::error!("Failed to count events: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(CountResponse { count: 0 }),
            )
        }
    }
}

// =============================================================================
// Response Types
// =============================================================================

#[derive(Serialize)]
pub struct EventResponse {
    pub id: i32,
    pub success: bool,
}

#[derive(Serialize)]
pub struct EventWithParsedPayload {
    pub id: i32,
    pub event_type: String,
    pub event_name: Option<String>,
    pub request_path: Option<String>,
    pub request_query: Option<String>,
    pub domain: Option<String>,
    pub session_id: Option<String>,
    pub request_id: Option<String>,
    pub payload: Value,
    pub received_at: String,
}

#[derive(Serialize)]
pub struct ClearResponse {
    pub deleted: u64,
    pub success: bool,
}

#[derive(Serialize)]
pub struct CountResponse {
    pub count: u64,
}
