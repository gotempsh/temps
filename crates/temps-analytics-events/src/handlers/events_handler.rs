use crate::services::AnalyticsEventsService;
use crate::types::{
    ActiveVisitorsQuery, ActiveVisitorsResponse, AggregatedBucketsResponse, AggregationLevel,
    EventCount, EventMetricsPayload, EventTimeline, EventTimelineQuery, EventTypeBreakdown,
    EventTypeBreakdownQuery, EventsCountQuery, HasEventsQuery, HasEventsResponse,
    HourlyVisitsQuery, PropertyBreakdownQuery, PropertyBreakdownResponse, PropertyColumn,
    PropertyTimelineQuery, PropertyTimelineResponse, SessionEventsQuery, SessionEventsResponse,
    UniqueCountsQuery, UniqueCountsResponse,
};
use axum::Extension;
use axum::{
    extract::{Path, Query, State},
    http::{StatusCode, header::HeaderMap},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_proxy::CachedPeerTable;
use tracing::error;

pub struct AppState {
    pub events_service: Arc<AnalyticsEventsService>,
    pub route_table: Arc<CachedPeerTable>,
    pub ip_address_service: Arc<temps_geo::IpAddressService>,
}

/// Get event counts with filtering
#[utoipa::path(
    get,
    path = "/projects/{project_id}/events",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("start_date" = String, Query, description = "Start date for filtering events"),
        ("end_date" = String, Query, description = "End date for filtering events"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
        ("limit" = Option<i32>, Query, description = "Maximum number of events to return (default: 20, max: 100)"),
        ("custom_events_only" = Option<bool>, Query, description = "Only return custom events, excluding system events like page_view, page_leave, heartbeat (default: true)"),
        ("aggregation_level" = Option<String>, Query, description = "Aggregation level: events, sessions, or visitors (default: events)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved event counts", body = Vec<EventCount>),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Events",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_events_count(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<EventsCountQuery>,
) -> Result<Json<Vec<EventCount>>, Problem> {
    permission_guard!(auth, AnalyticsRead);

    let events = state
        .events_service
        .get_events_count(
            query.start_date.into(),
            query.end_date.into(),
            project_id,
            query.environment_id,
            query.limit,
            query.custom_events_only,
            query.aggregation_level,
        )
        .await
        .map_err(|e| {
            error!("Failed to get event counts: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get event counts")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    Ok(Json(events))
}

/// Get events for a specific session
#[utoipa::path(
    get,
    path = "/sessions/{session_id}/events",
    params(
        ("session_id" = String, Path, description = "Session ID"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID")
    ),
    responses(
        (status = 200, description = "Successfully retrieved session events", body = SessionEventsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Session not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Events",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_session_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Query(query): Query<SessionEventsQuery>,
) -> Result<Json<SessionEventsResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);

    let events_response = state
        .events_service
        .get_session_events(session_id.clone(), query.project_id, query.environment_id)
        .await
        .map_err(|e| {
            error!("Failed to get session events: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get session events")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    match events_response {
        Some(events) => Ok(Json(events)),
        None => Err(ErrorBuilder::new(StatusCode::NOT_FOUND)
            .title("Session not found")
            .detail(format!("No events found for session: {}", session_id))
            .build()),
    }
}

/// Check if project has any analytics events
#[utoipa::path(
    get,
    path = "/projects/{project_id}/has-events",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, description = "Successfully checked for events", body = HasEventsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Events",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn has_analytics_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<HasEventsResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);

    let has_events = state
        .events_service
        .has_analytics_events(project_id, None)
        .await
        .map_err(|e| {
            error!("Failed to check for events: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to check for events")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    Ok(Json(HasEventsResponse { has_events }))
}

/// Get event type breakdown
#[utoipa::path(
    get,
    path = "/projects/{project_id}/events/breakdown",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("start_date" = String, Query, description = "Start date for filtering events"),
        ("end_date" = String, Query, description = "End date for filtering events"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
        ("aggregation_level" = Option<String>, Query, description = "Aggregation level: events, sessions, or visitors (default: events)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved event type breakdown", body = Vec<EventTypeBreakdown>),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Events",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_event_type_breakdown(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<EventTypeBreakdownQuery>,
) -> Result<Json<Vec<EventTypeBreakdown>>, Problem> {
    permission_guard!(auth, AnalyticsRead);

    let breakdown = state
        .events_service
        .get_event_type_breakdown(
            query.start_date.into(),
            query.end_date.into(),
            project_id,
            query.environment_id,
            query.aggregation_level,
        )
        .await
        .map_err(|e| {
            error!("Failed to get event type breakdown: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get event type breakdown")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    Ok(Json(breakdown))
}

/// Get events timeline
#[utoipa::path(
    get,
    path = "/projects/{project_id}/events/timeline",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("start_date" = String, Query, description = "Start date for filtering events"),
        ("end_date" = String, Query, description = "End date for filtering events"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
        ("event_name" = Option<String>, Query, description = "Filter by specific event name"),
        ("bucket_size" = Option<String>, Query, description = "Bucket size: hour, day, or week (auto-detected if not specified)"),
        ("aggregation_level" = Option<String>, Query, description = "Aggregation level: events, sessions, or visitors (default: events)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved events timeline", body = Vec<EventTimeline>),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Events",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_events_timeline(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<EventTimelineQuery>,
) -> Result<Json<Vec<EventTimeline>>, Problem> {
    permission_guard!(auth, AnalyticsRead);

    let timeline = state
        .events_service
        .get_events_timeline(
            query.start_date.into(),
            query.end_date.into(),
            project_id,
            query.environment_id,
            query.event_name,
            query.bucket_size,
            query.aggregation_level,
        )
        .await
        .map_err(|e| {
            error!("Failed to get events timeline: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get events timeline")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    Ok(Json(timeline))
}

/// Get active visitors count
#[utoipa::path(
    get,
    path = "/projects/{project_id}/active-visitors",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
        ("deployment_id" = Option<i32>, Query, description = "Filter by deployment ID")
    ),
    responses(
        (status = 200, description = "Successfully retrieved active visitors count", body = ActiveVisitorsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Events",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_active_visitors(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ActiveVisitorsQuery>,
) -> Result<Json<ActiveVisitorsResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);

    let active_count = state
        .events_service
        .get_active_visitors_count(project_id, query.environment_id, query.deployment_id)
        .await
        .map_err(|e| {
            error!("Failed to get active visitors: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get active visitors")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    Ok(Json(ActiveVisitorsResponse {
        active_visitors: active_count,
        window_minutes: 5,
    }))
}

/// Get hourly visits
#[utoipa::path(
    get,
    path = "/projects/{project_id}/hourly-visits",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("start_date" = String, Query, description = "Start date for filtering visits"),
        ("end_date" = String, Query, description = "End date for filtering visits"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
        ("aggregation_level" = Option<String>, Query, description = "Aggregation level: events (page views), sessions (unique sessions), or visitors (unique visitors) - default: events")
    ),
    responses(
        (status = 200, description = "Successfully retrieved hourly visits", body = Vec<EventTimeline>),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Events",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_hourly_visits(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<HourlyVisitsQuery>,
) -> Result<Json<Vec<EventTimeline>>, Problem> {
    permission_guard!(auth, AnalyticsRead);

    let hourly_data = state
        .events_service
        .get_hourly_visits(
            query.start_date.into(),
            query.end_date.into(),
            project_id,
            query.environment_id,
            query.aggregation_level,
        )
        .await
        .map_err(|e| {
            error!("Failed to get hourly visits: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get hourly visits")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    Ok(Json(hourly_data))
}

/// Get property breakdown by grouping events by a column
#[utoipa::path(
    get,
    path = "/projects/{project_id}/events/properties/breakdown",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("start_date" = String, Query, description = "Start date in '%Y-%m-%d %H:%M:%S' format"),
        ("end_date" = String, Query, description = "End date in '%Y-%m-%d %H:%M:%S' format"),
        ("group_by" = String, Query, description = "Column to group by (channel, device_type, browser, etc.)"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
        ("deployment_id" = Option<i32>, Query, description = "Filter by deployment ID"),
        ("event_name" = Option<String>, Query, description = "Filter by event name"),
        ("aggregation_level" = Option<String>, Query, description = "Aggregation level: events, sessions, or visitors - default: events"),
        ("limit" = Option<i32>, Query, description = "Maximum number of results (default: 20, max: 100)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved property breakdown", body = PropertyBreakdownResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Events",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_property_breakdown(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<PropertyBreakdownQuery>,
) -> Result<Json<PropertyBreakdownResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);

    let aggregation_level = query.aggregation_level.as_str();

    let breakdown = state
        .events_service
        .get_property_breakdown(
            query.start_date.into(),
            query.end_date.into(),
            project_id,
            query.environment_id,
            query.deployment_id,
            query.event_name,
            query.group_by.clone(),
            aggregation_level,
            query.limit,
        )
        .await
        .map_err(|e| {
            error!("Failed to get property breakdown: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get property breakdown")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    Ok(Json(breakdown))
}

/// Get property timeline by grouping events by a column over time
#[utoipa::path(
    get,
    path = "/projects/{project_id}/events/properties/timeline",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("start_date" = String, Query, description = "Start date in '%Y-%m-%d %H:%M:%S' format"),
        ("end_date" = String, Query, description = "End date in '%Y-%m-%d %H:%M:%S' format"),
        ("group_by" = String, Query, description = "Column to group by (channel, device_type, browser, etc.)"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
        ("deployment_id" = Option<i32>, Query, description = "Filter by deployment ID"),
        ("event_name" = Option<String>, Query, description = "Filter by event name"),
        ("aggregation_level" = Option<String>, Query, description = "Aggregation level: events, sessions, or visitors - default: events"),
        ("bucket_size" = Option<String>, Query, description = "Time bucket: hour, day, week, month (default: auto-detect)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved property timeline", body = PropertyTimelineResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Events",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_property_timeline(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<PropertyTimelineQuery>,
) -> Result<Json<PropertyTimelineResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);

    let aggregation_level = query.aggregation_level.as_str();

    let timeline = state
        .events_service
        .get_property_timeline(
            query.start_date.into(),
            query.end_date.into(),
            project_id,
            query.environment_id,
            query.deployment_id,
            query.event_name,
            query.group_by.clone(),
            aggregation_level,
            query.bucket_size,
        )
        .await
        .map_err(|e| {
            error!("Failed to get property timeline: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get property timeline")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    Ok(Json(timeline))
}

/// Get unique counts over time frame
#[utoipa::path(
    get,
    path = "/projects/{project_id}/unique-counts",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("start_date" = String, Query, description = "Start date in '%Y-%m-%d %H:%M:%S' format"),
        ("end_date" = String, Query, description = "End date in '%Y-%m-%d %H:%M:%S' format"),
        ("environment_id" = Option<i32>, Query, description = "Filter by environment ID"),
        ("deployment_id" = Option<i32>, Query, description = "Filter by deployment ID"),
        ("metric" = String, Query, description = "Metric to count: 'sessions' (unique sessions), 'visitors' (unique visitors), or 'page_views' (total page views) (default: 'sessions')")
    ),
    responses(
        (status = 200, description = "Successfully retrieved count", body = UniqueCountsResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Events",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_unique_counts(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<UniqueCountsQuery>,
) -> Result<Json<UniqueCountsResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);

    let counts = state
        .events_service
        .get_unique_counts(
            query.start_date.into(),
            query.end_date.into(),
            project_id,
            query.environment_id,
            query.deployment_id,
            query.metric.to_lowercase(),
        )
        .await
        .map_err(|e| {
            error!("Failed to get unique counts: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get unique counts")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    Ok(Json(counts))
}

/// Record analytics event
#[utoipa::path(
    tag = "Metrics",
    post,
    path = "/_temps/event",
    request_body = EventMetricsPayload,
    responses(
        (status = 200, description = "Event recorded successfully"),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn record_event_metrics(
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    headers: HeaderMap,
    Json(payload): Json<EventMetricsPayload>,
) -> impl IntoResponse {
    use tracing::{error, info};

    info!(
        "Recording event metrics: {} path: {}",
        payload.event_name,
        payload.request_path
    );

    // Extract domain from Host header
    let host = match headers.get("host") {
        Some(host) => match host.to_str() {
            Ok(host_str) => host_str.to_string(),
            Err(_) => {
                error!("Invalid Host header");
                return StatusCode::BAD_REQUEST.into_response();
            }
        },
        None => {
            error!("Missing Host header");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };

    // Look up project/environment/deployment from route table o(1)
    let (project_id, environment_id, deployment_id) = match state.route_table.get_route(&host) {
        Some(route_info) => {
            let project_id = route_info.project.as_ref().map(|p| p.id).unwrap_or(1);
            let environment_id = route_info.environment.as_ref().map(|e| e.id);
            let deployment_id = route_info.deployment.as_ref().map(|d| d.id);

            info!(
                "Resolved host {} to project={}, env={:?}, deploy={:?}",
                host, project_id, environment_id, deployment_id
            );

            (project_id, environment_id, deployment_id)
        }
        None => {
            error!("Host {} not found in route table", host);
            // Return 404 or BAD_REQUEST since we can't track events for unknown hosts
            return StatusCode::NOT_FOUND.into_response();
        }
    };

    // Extract user agent and referrer from headers
    let user_agent = headers.get("user-agent")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    let referrer_header = headers.get("referer")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    // Use payload referrer if provided, otherwise fall back to header
    let referrer = payload.referrer.or(referrer_header);

    // Extract language from event_data if not provided in payload
    let language = payload.language.or_else(|| {
        payload.event_data.get("language")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });

    // Lookup IP geolocation
    let ip_geolocation_id = if !metadata.ip_address.is_empty() {
        match state.ip_address_service.get_or_create_ip(&metadata.ip_address).await {
            Ok(ip_info) => {
                info!("Resolved IP {} to geolocation: country={:?}, city={:?}",
                      metadata.ip_address, ip_info.country, ip_info.city);
                Some(ip_info.id)
            }
            Err(e) => {
                error!("Failed to lookup IP geolocation for {}: {}", metadata.ip_address, e);
                None
            }
        }
    } else {
        None
    };

    match state
        .events_service
        .record_event(
            project_id,
            environment_id,
            deployment_id,
            metadata.session_id_cookie,
            metadata.visitor_id_cookie,
            &payload.event_name,
            payload.event_data,
            &payload.request_path,
            &payload.request_query,
            payload.screen_width,
            payload.screen_height,
            payload.viewport_width,
            payload.viewport_height,
            language,
            payload.page_title,
            ip_geolocation_id,
            user_agent,
            referrer,
            // Performance metrics (web vitals)
            payload.ttfb,
            payload.lcp,
            payload.fid,
            payload.fcp,
            payload.cls,
            payload.inp,
        )
        .await
    {
        Ok(_) => {
            info!(
                "Event recorded: {} for host: {} path: {} (project={}, env={:?}, deploy={:?})",
                payload.event_name,
                host,
                payload.request_path,
                project_id,
                environment_id,
                deployment_id
            );
            StatusCode::OK.into_response()
        }
        Err(e) => {
            error!("Failed to record event: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Get aggregated metrics by time bucket
#[utoipa::path(
    get,
    path = "/projects/{project_id}/aggregated-buckets",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("start_date" = String, Query, description = "Start date for the query range"),
        ("end_date" = String, Query, description = "End date for the query range"),
        ("environment_id" = Option<i32>, Query, description = "Optional environment filter"),
        ("deployment_id" = Option<i32>, Query, description = "Optional deployment filter"),
        ("aggregation_level" = Option<String>, Query, description = "Aggregation level: events, sessions, or visitors (default: events)"),
        ("bucket_size" = Option<String>, Query, description = "Time bucket size: '1 hour', '1 day', '1 week', etc. (default: '1 hour')")
    ),
    responses(
        (status = 200, description = "Successfully retrieved aggregated buckets", body = AggregatedBucketsResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Events",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_aggregated_buckets(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<crate::types::AggregatedBucketsQuery>,
) -> Result<Json<crate::types::AggregatedBucketsResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);

    let result = state
        .events_service
        .get_aggregated_buckets(
            query.start_date.into(),
            query.end_date.into(),
            project_id,
            query.environment_id,
            query.deployment_id,
            query.aggregation_level,
            query.bucket_size,
        )
        .await
        .map_err(|e| {
            error!("Failed to get aggregated buckets: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get aggregated buckets")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    Ok(Json(result))
}

/// Configure routes for events
pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/projects/{project_id}/events", get(get_events_count))
        .route("/projects/{project_id}/events/breakdown", get(get_event_type_breakdown))
        .route("/projects/{project_id}/events/timeline", get(get_events_timeline))
        .route("/projects/{project_id}/events/properties/breakdown", get(get_property_breakdown))
        .route("/projects/{project_id}/events/properties/timeline", get(get_property_timeline))
        .route("/projects/{project_id}/aggregated-buckets", get(get_aggregated_buckets))
        .route("/projects/{project_id}/unique-counts", get(get_unique_counts))
        .route("/projects/{project_id}/active-visitors", get(get_active_visitors))
        .route("/projects/{project_id}/hourly-visits", get(get_hourly_visits))
        .route("/projects/{project_id}/has-events", get(has_analytics_events))
        .route(
            "/sessions/{session_id}/events",
            get(get_session_events),
        )
        .route("/_temps/event", post(record_event_metrics))
}

#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        get_events_count,
        get_event_type_breakdown,
        get_events_timeline,
        get_property_breakdown,
        get_property_timeline,
        get_aggregated_buckets,
        get_unique_counts,
        get_active_visitors,
        get_hourly_visits,
        record_event_metrics,
        get_session_events,
        has_analytics_events,
    ),
    components(
        schemas(
            EventCount,
            EventsCountQuery,
            EventTypeBreakdown,
            EventTypeBreakdownQuery,
            EventTimeline,
            EventTimelineQuery,
            PropertyBreakdownQuery,
            PropertyBreakdownResponse,
            PropertyTimelineQuery,
            PropertyTimelineResponse,
            PropertyColumn,
            AggregationLevel,
            UniqueCountsQuery,
            UniqueCountsResponse,
            crate::types::AggregatedBucketsQuery,
            crate::types::AggregatedBucketsResponse,
            crate::types::AggregatedBucketItem,
            ActiveVisitorsResponse,
            ActiveVisitorsQuery,
            HourlyVisitsQuery,
            EventMetricsPayload,
            SessionEventsResponse,
            SessionEventsQuery,
            HasEventsResponse,
            HasEventsQuery,
        )
    ),
    tags(
        (name = "Events", description = "Analytics events tracking endpoints"),
        (name = "Metrics", description = "Analytics metrics collection endpoints including performance web vitals")
    )
)]
pub struct EventsApiDoc;
