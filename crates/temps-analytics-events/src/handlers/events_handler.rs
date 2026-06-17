use crate::services::{
    queries::{
        ActiveVisitorsSpec, AggregatedBucketsSpec, AnalyticsScope, DashboardProjectsSpec,
        EventTypeBreakdownSpec, EventsCountSpec, EventsTimelineSpec, HasEventsSpec,
        HourlyVisitsSpec, PropertyBreakdownSpec, PropertyTimelineSpec, SessionEventsSpec,
        TimeRange, UniqueCountsSpec,
    },
    AnalyticsEvents, AnalyticsEventsService,
};
use crate::types::{
    ActiveVisitorsQuery, ActiveVisitorsResponse, AggregatedBucketsResponse, AggregationLevel,
    AnalyticsSessionEventsResponse, ConsoleEventPayload, EventCount, EventMetricsPayload,
    EventTimeline, EventTimelineQuery, EventTypeBreakdown, EventTypeBreakdownQuery,
    EventsCountQuery, HasEventsQuery, HasEventsResponse, HourlyVisitsQuery, PropertyBreakdownQuery,
    PropertyBreakdownResponse, PropertyColumn, PropertyTimelineQuery, PropertyTimelineResponse,
    SessionEventsQuery, UniqueCountsQuery, UniqueCountsResponse,
};
use axum::Extension;
use axum::{
    extract::{Path, Query, State},
    http::{header::HeaderMap, StatusCode},
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
    /// Read-side: queries dispatched through the trait so the storage backend
    /// can swap (TimescaleDB today, ClickHouse later) without handler edits.
    pub events_service: Arc<dyn AnalyticsEvents>,
    /// Write-side: stays a concrete service. Writes don't pick a backend at the
    /// query level; they go to PG and fan out to CH via the outbox in Phase 2.
    pub events_writer: Arc<AnalyticsEventsService>,
    pub route_table: Arc<CachedPeerTable>,
    pub ip_address_service: Arc<temps_geo::IpAddressService>,
    pub cookie_crypto: Arc<temps_core::CookieCrypto>,
    pub telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
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

    let spec = EventsCountSpec::new(
        TimeRange {
            start: query.start_date.into(),
            end: query.end_date.into(),
        },
        AnalyticsScope::project(project_id).with_environment(query.environment_id),
        query.aggregation_level,
        query.limit,
        query.custom_events_only,
    );
    let events = state
        .events_service
        .query_events_count(spec)
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
        (status = 200, description = "Successfully retrieved session events", body = AnalyticsSessionEventsResponse),
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
) -> Result<Json<AnalyticsSessionEventsResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);

    let spec = SessionEventsSpec {
        session_id: session_id.clone(),
        scope: AnalyticsScope::project(query.project_id).with_environment(query.environment_id),
    };
    let events_response = state
        .events_service
        .query_session_events(spec)
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

    let spec = HasEventsSpec {
        scope: AnalyticsScope::project(project_id),
    };
    let has_events = state
        .events_service
        .query_has_events(spec)
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

    let spec = EventTypeBreakdownSpec {
        range: TimeRange {
            start: query.start_date.into(),
            end: query.end_date.into(),
        },
        scope: AnalyticsScope::project(project_id).with_environment(query.environment_id),
        aggregation_level: query.aggregation_level,
    };
    let breakdown = state
        .events_service
        .query_event_type_breakdown(spec)
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

    let spec = EventsTimelineSpec {
        range: TimeRange {
            start: query.start_date.into(),
            end: query.end_date.into(),
        },
        scope: AnalyticsScope::project(project_id).with_environment(query.environment_id),
        aggregation_level: query.aggregation_level,
        event_name: query.event_name,
        bucket_size: query.bucket_size,
    };
    let timeline = state
        .events_service
        .query_events_timeline(spec)
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

    let spec = ActiveVisitorsSpec {
        scope: AnalyticsScope::project(project_id)
            .with_environment(query.environment_id)
            .with_deployment(query.deployment_id),
    };
    let active_count = state
        .events_service
        .query_active_visitors(spec)
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

    let spec = HourlyVisitsSpec {
        range: TimeRange {
            start: query.start_date.into(),
            end: query.end_date.into(),
        },
        scope: AnalyticsScope::project(project_id).with_environment(query.environment_id),
        aggregation_level: query.aggregation_level,
    };
    let hourly_data = state
        .events_service
        .query_hourly_visits(spec)
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
        ("limit" = Option<i32>, Query, description = "Maximum number of results (default: 20, max: 100)"),
        ("filter_country" = Option<String>, Query, description = "Filter by country (for region/city drill-downs)"),
        ("filter_region" = Option<String>, Query, description = "Filter by region (for city drill-downs)"),
        ("filter_browser" = Option<String>, Query, description = "Filter by browser name (for version drill-downs)"),
        ("filter_os" = Option<String>, Query, description = "Filter by OS name (for version drill-downs)"),
        ("filter_channel" = Option<String>, Query, description = "Filter by channel name (for channel drill-downs)"),
        ("filter_referrer" = Option<String>, Query, description = "Filter by referrer hostname (for referrer drill-downs)")
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

    let filters = crate::types::PropertyBreakdownFilters {
        country: query.filter_country,
        region: query.filter_region,
        browser: query.filter_browser,
        operating_system: query.filter_os,
        channel: query.filter_channel,
        referrer: query.filter_referrer,
    };

    let spec = PropertyBreakdownSpec::new(
        TimeRange {
            start: query.start_date.into(),
            end: query.end_date.into(),
        },
        AnalyticsScope::project(project_id)
            .with_environment(query.environment_id)
            .with_deployment(query.deployment_id),
        query.event_name,
        query.group_by.clone(),
        aggregation_level,
        query.limit,
        Some(filters),
    );
    let breakdown = state
        .events_service
        .query_property_breakdown(spec)
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

    let spec = PropertyTimelineSpec {
        range: TimeRange {
            start: query.start_date.into(),
            end: query.end_date.into(),
        },
        scope: AnalyticsScope::project(project_id)
            .with_environment(query.environment_id)
            .with_deployment(query.deployment_id),
        event_name: query.event_name,
        group_by_column: query.group_by.clone(),
        aggregation_level: aggregation_level.to_string(),
        bucket_size: query.bucket_size,
    };
    let timeline = state
        .events_service
        .query_property_timeline(spec)
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

    let spec = UniqueCountsSpec {
        range: TimeRange {
            start: query.start_date.into(),
            end: query.end_date.into(),
        },
        scope: AnalyticsScope::project(project_id)
            .with_environment(query.environment_id)
            .with_deployment(query.deployment_id),
        metric: query.metric.to_lowercase(),
    };
    let counts = state
        .events_service
        .query_unique_counts(spec)
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
        (status = 204, description = "Event recorded successfully"),
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
        payload.event_name, payload.request_path
    );

    // Resolve the host from request metadata. The middleware has already
    // stripped the ":port" suffix so it can be used as a route-table key
    // directly — a raw Host header would break on non-default ports like the
    // local dev proxy's :8080, which is what the route table never contains.
    let host = metadata.host.clone();
    if host.is_empty() {
        error!("Missing Host header");
        return StatusCode::BAD_REQUEST.into_response();
    }

    // Look up project/environment/deployment from route table o(1)
    let (project_id, environment_id, deployment_id) = match state.route_table.get_route(&host) {
        Some(route_info) => {
            // A route without a project is a sandbox/orphaned route — we can't
            // attribute the event to anything, so silently drop it (204) rather
            // than falling back to project_id=1 which FK-violates on insert.
            let Some(project) = route_info.project.as_ref() else {
                info!(
                    "Dropping event for host {} — route has no associated project (sandbox/orphan)",
                    host
                );
                return StatusCode::NO_CONTENT.into_response();
            };

            let project_id = project.id;
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
    let user_agent = headers
        .get("user-agent")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    let referrer_header = headers
        .get("referer")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    // Priority for referrer:
    // 1. event_data.referrer - the actual referrer from where the user came (captured by JS)
    // 2. payload.referrer - top-level referrer field if provided
    // 3. HTTP Referer header - fallback (usually just the current page making the request)
    let event_data_referrer = payload
        .event_data
        .get("referrer")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let referrer = event_data_referrer
        .or(payload.referrer.clone())
        .or(referrer_header);

    // Extract language from event_data if not provided in payload
    let language = payload.language.or_else(|| {
        payload
            .event_data
            .get("language")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });

    // Lookup IP geolocation
    let ip_geolocation_id = if !metadata.ip_address.is_empty() {
        match state
            .ip_address_service
            .get_or_create_ip(&metadata.ip_address)
            .await
        {
            Ok(ip_info) => {
                info!(
                    "Resolved IP {} to geolocation: country={:?}, city={:?}",
                    metadata.ip_address, ip_info.country, ip_info.city
                );
                Some(ip_info.id)
            }
            Err(e) => {
                error!(
                    "Failed to lookup IP geolocation for {}: {}",
                    metadata.ip_address, e
                );
                None
            }
        }
    } else {
        None
    };

    match state
        .events_writer
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
            state
                .telemetry
                .report(temps_core::telemetry::TelemetryEvent::new(
                    temps_core::telemetry::TelemetryEventKind::AnalyticsFirstEventReceived,
                ));
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            error!("Failed to record event: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Record an analytics event via the console API with explicit project ID.
///
/// The app backend forwards the user's encrypted Temps cookies, so visitor/session
/// identity is resolved automatically by middleware. No geolocation or user-agent
/// enrichment is performed — this is a lightweight server-side ingestion path.
#[utoipa::path(
    tag = "Events",
    post,
    path = "/projects/{project_id}/events/ingest",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    request_body = ConsoleEventPayload,
    responses(
        (status = 200, description = "Event recorded successfully"),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn record_console_event(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
    Json(payload): Json<ConsoleEventPayload>,
) -> Result<impl IntoResponse, Problem> {
    use tracing::{info, warn};

    permission_guard!(auth, AnalyticsWrite);

    info!(
        "Recording console event: {} for project {} path: {}",
        payload.event_name, project_id, payload.request_path
    );

    // Decrypt the encrypted cookie values if provided.
    // Generate fallback UUIDs when cookies are absent or decryption fails,
    // because the events hypertable enforces NOT NULL on session_id.
    let visitor_id = payload.visitor_id.as_deref().and_then(|encrypted| {
        match state.cookie_crypto.decrypt(encrypted) {
            Ok(decrypted) => Some(decrypted),
            Err(e) => {
                warn!(
                    "Failed to decrypt visitor_id cookie for project {}: {}",
                    project_id, e
                );
                None
            }
        }
    });

    let session_id = payload
        .session_id
        .as_deref()
        .and_then(|encrypted| match state.cookie_crypto.decrypt(encrypted) {
            Ok(decrypted) => Some(decrypted),
            Err(e) => {
                warn!(
                    "Failed to decrypt session_id cookie for project {}: {}",
                    project_id, e
                );
                None
            }
        })
        .or_else(|| Some(temps_core::uuid::Uuid::new_v4().to_string()));

    state
        .events_writer
        .record_event(
            project_id,
            Some(payload.environment_id),
            Some(payload.deployment_id),
            session_id,
            visitor_id,
            &payload.event_name,
            payload.event_data,
            &payload.request_path,
            &payload.request_query,
            None, // screen_width
            None, // screen_height
            None, // viewport_width
            None, // viewport_height
            None, // language
            None, // page_title
            None, // ip_geolocation_id
            None, // user_agent
            None, // referrer
            None, // ttfb
            None, // lcp
            None, // fid
            None, // fcp
            None, // cls
            None, // inp
        )
        .await
        .map_err(|e| {
            error!("Failed to record console event: {:?}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to record event")
                .detail(format!(
                    "Error recording event for project {}: {}",
                    project_id, e
                ))
                .build()
        })?;

    info!(
        "Console event recorded: {} for project {} env={}",
        payload.event_name, project_id, payload.environment_id
    );
    state
        .telemetry
        .report(temps_core::telemetry::TelemetryEvent::new(
            temps_core::telemetry::TelemetryEventKind::AnalyticsFirstEventReceived,
        ));

    Ok(StatusCode::OK)
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

    let spec = AggregatedBucketsSpec {
        range: TimeRange {
            start: query.start_date.into(),
            end: query.end_date.into(),
        },
        scope: AnalyticsScope::project(project_id)
            .with_environment(query.environment_id)
            .with_deployment(query.deployment_id),
        aggregation_level: query.aggregation_level,
        bucket_size: query.bucket_size,
    };
    let result = state
        .events_service
        .query_aggregated_buckets(spec)
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

/// Get dashboard analytics for multiple projects in a single batch request
///
/// Returns unique visitor counts and hourly sparkline data for all requested projects
/// using only 2 SQL queries instead of 2×N per-project queries.
#[utoipa::path(
    get,
    path = "/dashboard/projects-analytics",
    params(
        ("project_ids" = String, Query, description = "Comma-separated list of project IDs"),
        ("start_date" = String, Query, description = "Start date for filtering"),
        ("end_date" = String, Query, description = "End date for filtering"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved batch analytics", body = crate::types::DashboardProjectsAnalyticsResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Events",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_dashboard_projects_analytics(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<crate::types::DashboardProjectsAnalyticsQuery>,
) -> Result<Json<crate::types::DashboardProjectsAnalyticsResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);

    let project_ids: Vec<i32> = query
        .project_ids
        .split(',')
        .filter_map(|s| s.trim().parse::<i32>().ok())
        .collect();

    if project_ids.is_empty() {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Invalid project IDs")
            .detail("project_ids must contain at least one valid integer")
            .build());
    }

    if project_ids.len() > 100 {
        return Err(ErrorBuilder::new(StatusCode::BAD_REQUEST)
            .title("Too many project IDs")
            .detail("Maximum 100 project IDs per request")
            .build());
    }

    let spec = DashboardProjectsSpec {
        project_ids: project_ids.clone(),
        range: TimeRange {
            start: query.start_date.into(),
            end: query.end_date.into(),
        },
    };
    let result = state
        .events_service
        .query_dashboard_projects(spec)
        .await
        .map_err(|e| {
            error!("Failed to get dashboard projects analytics: {}", e);
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to get dashboard analytics")
                .detail(format!("Error: {}", e))
                .build()
        })?;

    Ok(Json(result))
}

/// Configure admin routes for events (authenticated queries / management).
pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/dashboard/projects-analytics",
            get(get_dashboard_projects_analytics),
        )
        .route("/projects/{project_id}/events", get(get_events_count))
        .route(
            "/projects/{project_id}/events/breakdown",
            get(get_event_type_breakdown),
        )
        .route(
            "/projects/{project_id}/events/timeline",
            get(get_events_timeline),
        )
        .route(
            "/projects/{project_id}/events/properties/breakdown",
            get(get_property_breakdown),
        )
        .route(
            "/projects/{project_id}/events/properties/timeline",
            get(get_property_timeline),
        )
        .route(
            "/projects/{project_id}/aggregated-buckets",
            get(get_aggregated_buckets),
        )
        .route(
            "/projects/{project_id}/unique-counts",
            get(get_unique_counts),
        )
        .route(
            "/projects/{project_id}/active-visitors",
            get(get_active_visitors),
        )
        .route(
            "/projects/{project_id}/hourly-visits",
            get(get_hourly_visits),
        )
        .route(
            "/projects/{project_id}/has-events",
            get(has_analytics_events),
        )
        .route(
            "/projects/{project_id}/events/ingest",
            post(record_console_event),
        )
        .route("/sessions/{session_id}/events", get(get_session_events))
}

/// Configure public ingest routes for events.
///
/// These are called by browser SDKs on customer sites and must be reachable
/// without authentication — the project is resolved from the Host header.
pub fn configure_public_routes() -> Router<Arc<AppState>> {
    Router::new().route("/_temps/event", post(record_event_metrics))
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
        record_console_event,
        get_session_events,
        has_analytics_events,
        get_dashboard_projects_analytics,
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
            ConsoleEventPayload,
            AnalyticsSessionEventsResponse,
            SessionEventsQuery,
            HasEventsResponse,
            HasEventsQuery,
            crate::types::DashboardProjectsAnalyticsQuery,
            crate::types::DashboardProjectsAnalyticsResponse,
            crate::types::ProjectDashboardAnalytics,
        )
    ),
    tags(
        (name = "Events", description = "Analytics events tracking endpoints"),
        (name = "Metrics", description = "Analytics metrics collection endpoints including performance web vitals")
    )
)]
pub struct EventsApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::middleware;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::projects;
    use tower::ServiceExt;

    fn create_test_auth_context() -> temps_auth::AuthContext {
        let user = temps_entities::users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: Some("hashed".to_string()),
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        temps_auth::AuthContext::new_session(user, temps_auth::Role::Admin)
    }

    async fn setup_test_app(
        db: Arc<sea_orm::DatabaseConnection>,
    ) -> (axum::Router, Arc<AppState>, Arc<temps_core::CookieCrypto>) {
        let events_writer = Arc::new(crate::services::AnalyticsEventsService::new(db.clone()));
        let events_service: Arc<dyn crate::services::AnalyticsEvents> = events_writer.clone();
        let route_table = Arc::new(temps_proxy::CachedPeerTable::new(db.clone()));
        let geoip_service = Arc::new(temps_geo::GeoIpService::Mock(
            temps_geo::MockGeoIpService::new(),
        ));
        let ip_address_service =
            Arc::new(temps_geo::IpAddressService::new(db.clone(), geoip_service));
        let cookie_crypto =
            Arc::new(temps_core::CookieCrypto::new("test_key_32_bytes_long_for_tests").unwrap());

        let app_state = Arc::new(AppState {
            events_service,
            events_writer,
            route_table,
            ip_address_service,
            cookie_crypto: cookie_crypto.clone(),
            telemetry: Arc::new(temps_core::telemetry::NoopTelemetryReporter),
        });

        let auth_middleware = middleware::from_fn(
            |mut req: Request<Body>, next: axum::middleware::Next| async move {
                let auth_context = create_test_auth_context();
                req.extensions_mut().insert(auth_context);
                next.run(req).await
            },
        );

        let app = configure_routes()
            .layer(auth_middleware)
            .with_state(app_state.clone());

        (app, app_state, cookie_crypto)
    }

    async fn insert_test_environment(
        db: &sea_orm::DatabaseConnection,
        project_id: i32,
    ) -> temps_entities::environments::Model {
        use temps_entities::{environments, upstream_config::UpstreamList};
        environments::ActiveModel {
            project_id: Set(project_id),
            name: Set("production".to_string()),
            branch: Set(Some("main".to_string())),
            slug: Set("production".to_string()),
            subdomain: Set("prod".to_string()),
            host: Set(String::new()),
            upstreams: Set(UpstreamList::new()),
            is_preview: Set(false),
            current_deployment_id: Set(None),
            deleted_at: Set(None),
            deployment_config: Set(None),
            last_deployment: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("Failed to insert test environment")
    }

    async fn insert_test_deployment(
        db: &sea_orm::DatabaseConnection,
        project_id: i32,
        environment_id: i32,
    ) -> temps_entities::deployments::Model {
        use temps_entities::deployments;
        deployments::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            slug: Set(format!("test-deploy-{}", uuid::Uuid::new_v4())),
            state: Set("ready".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            deploying_at: Set(None),
            ready_at: Set(Some(chrono::Utc::now())),
            started_at: Set(Some(chrono::Utc::now())),
            finished_at: Set(Some(chrono::Utc::now())),
            context_vars: Set(None),
            branch_ref: Set(Some("main".to_string())),
            tag_ref: Set(None),
            commit_sha: Set(None),
            commit_message: Set(None),
            commit_author: Set(None),
            commit_json: Set(None),
            cancelled_reason: Set(None),
            static_dir_location: Set(None),
            screenshot_location: Set(None),
            image_name: Set(None),
            deployment_config: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("Failed to insert test deployment")
    }

    async fn insert_test_project(db: &sea_orm::DatabaseConnection) -> projects::Model {
        projects::ActiveModel {
            name: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            preset_config: Set(None),
            deployment_config: Set(None),
            slug: Set("test-project".to_string()),
            is_deleted: Set(false),
            deleted_at: Set(None),
            last_deployment: Set(None),
            is_public_repo: Set(false),
            git_url: Set(None),
            git_provider_connection_id: Set(None),
            attack_mode: Set(false),
            enable_preview_environments: Set(false),
            source_type: Set(temps_entities::source_type::SourceType::Git),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("Failed to insert test project")
    }

    #[tokio::test]
    async fn test_console_event_ingest_success() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let environment = insert_test_environment(db.as_ref(), project.id).await;
        let deployment = insert_test_deployment(db.as_ref(), project.id, environment.id).await;
        let (app, _state, _crypto) = setup_test_app(db.clone()).await;

        let payload = serde_json::json!({
            "event_name": "purchase",
            "event_data": { "plan": "pro", "amount": 49.99 },
            "environment_id": environment.id,
            "deployment_id": deployment.id,
            "request_path": "/checkout",
            "request_query": ""
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/projects/{}/events/ingest", project.id))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "Expected 200, got {}. Body: {}",
            status,
            String::from_utf8_lossy(&body)
        );

        // Verify the event was stored in the database
        let events: Vec<temps_entities::events::Model> = temps_entities::events::Entity::find()
            .filter(temps_entities::events::Column::ProjectId.eq(project.id))
            .all(db.as_ref())
            .await
            .expect("Failed to query events");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_name.as_deref(), Some("purchase"));
        assert_eq!(events[0].pathname, "/checkout");
        assert_eq!(events[0].project_id, project.id);
        assert_eq!(events[0].environment_id, Some(environment.id));
        assert!(events[0].visitor_id.is_none());

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn test_console_event_with_encrypted_visitor_id() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        use temps_entities::visitor;
        let environment = insert_test_environment(db.as_ref(), project.id).await;
        let deployment = insert_test_deployment(db.as_ref(), project.id, environment.id).await;
        let (app, _state, cookie_crypto) = setup_test_app(db.clone()).await;

        // Create a visitor in the DB first
        let visitor_uuid = uuid::Uuid::new_v4().to_string();
        let _visitor = visitor::ActiveModel {
            visitor_id: Set(visitor_uuid.clone()),
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            first_seen: Set(chrono::Utc::now()),
            last_seen: Set(chrono::Utc::now()),
            has_activity: Set(false),
            is_crawler: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("Failed to insert test visitor");

        // Encrypt the visitor_id like the browser cookie would have
        let encrypted_visitor_id = cookie_crypto.encrypt(&visitor_uuid).unwrap();
        let session_uuid = uuid::Uuid::new_v4().to_string();
        let encrypted_session_id = cookie_crypto.encrypt(&session_uuid).unwrap();

        let payload = serde_json::json!({
            "event_name": "add_to_cart",
            "event_data": { "item": "widget" },
            "visitor_id": encrypted_visitor_id,
            "session_id": encrypted_session_id,
            "environment_id": environment.id,
            "deployment_id": deployment.id,
            "request_path": "/products/widget"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/projects/{}/events/ingest", project.id))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Verify the event was stored with the correct visitor
        let events: Vec<temps_entities::events::Model> = temps_entities::events::Entity::find()
            .filter(temps_entities::events::Column::ProjectId.eq(project.id))
            .all(db.as_ref())
            .await
            .expect("Failed to query events");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_name.as_deref(), Some("add_to_cart"));
        assert_eq!(events[0].pathname, "/products/widget");
        // Visitor should be resolved from the encrypted cookie
        assert!(events[0].visitor_id.is_some());
        // Session should be decrypted and stored
        assert_eq!(events[0].session_id.as_deref(), Some(session_uuid.as_str()));

        // Verify visitor's has_activity was updated
        let updated_visitor: temps_entities::visitor::Model = visitor::Entity::find()
            .filter(visitor::Column::VisitorId.eq(&visitor_uuid))
            .one(db.as_ref())
            .await
            .expect("Failed to query visitor")
            .expect("Visitor not found");
        assert!(updated_visitor.has_activity);

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn test_console_event_with_invalid_encrypted_cookies_still_succeeds() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let environment = insert_test_environment(db.as_ref(), project.id).await;
        let deployment = insert_test_deployment(db.as_ref(), project.id, environment.id).await;
        let (app, _state, _crypto) = setup_test_app(db.clone()).await;

        // Send garbage encrypted values — should warn but not fail
        let payload = serde_json::json!({
            "event_name": "page_view",
            "environment_id": environment.id,
            "deployment_id": deployment.id,
            "visitor_id": "not_a_valid_encrypted_value",
            "session_id": "also_garbage"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/projects/{}/events/ingest", project.id))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should succeed — invalid cookies are treated as absent
        assert_eq!(response.status(), StatusCode::OK);

        let events: Vec<temps_entities::events::Model> = temps_entities::events::Entity::find()
            .filter(temps_entities::events::Column::ProjectId.eq(project.id))
            .all(db.as_ref())
            .await
            .expect("Failed to query events");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_name.as_deref(), Some("page_view"));
        assert!(events[0].visitor_id.is_none());
        // session_id gets a generated UUID fallback when cookie decryption fails
        assert!(events[0].session_id.is_some());

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn test_console_event_with_environment_id() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let environment = insert_test_environment(db.as_ref(), project.id).await;
        let deployment = insert_test_deployment(db.as_ref(), project.id, environment.id).await;
        let (app, _state, _crypto) = setup_test_app(db.clone()).await;

        let payload = serde_json::json!({
            "event_name": "deploy_complete",
            "event_data": { "version": "1.2.3" },
            "environment_id": environment.id,
            "deployment_id": deployment.id,
            "request_path": "/deploy"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/projects/{}/events/ingest", project.id))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let events: Vec<temps_entities::events::Model> = temps_entities::events::Entity::find()
            .filter(temps_entities::events::Column::ProjectId.eq(project.id))
            .all(db.as_ref())
            .await
            .expect("Failed to query events");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].environment_id, Some(environment.id));
        assert_eq!(events[0].event_name.as_deref(), Some("deploy_complete"));

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn test_console_event_without_auth_returns_401() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;

        let events_writer = Arc::new(crate::services::AnalyticsEventsService::new(db.clone()));
        let events_service: Arc<dyn crate::services::AnalyticsEvents> = events_writer.clone();
        let route_table = Arc::new(temps_proxy::CachedPeerTable::new(db.clone()));
        let geoip_service = Arc::new(temps_geo::GeoIpService::Mock(
            temps_geo::MockGeoIpService::new(),
        ));
        let ip_address_service =
            Arc::new(temps_geo::IpAddressService::new(db.clone(), geoip_service));
        let cookie_crypto =
            Arc::new(temps_core::CookieCrypto::new("test_key_32_bytes_long_for_tests").unwrap());
        let app_state = Arc::new(AppState {
            events_service,
            events_writer,
            route_table,
            ip_address_service,
            cookie_crypto,
            telemetry: Arc::new(temps_core::telemetry::NoopTelemetryReporter),
        });

        // No auth middleware — should return 401
        let app = configure_routes().with_state(app_state);

        let payload = serde_json::json!({
            "event_name": "should_fail"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/projects/{}/events/ingest", project.id))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn test_console_event_minimal_payload() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let environment = insert_test_environment(db.as_ref(), project.id).await;
        let deployment = insert_test_deployment(db.as_ref(), project.id, environment.id).await;
        let (app, _state, _crypto) = setup_test_app(db.clone()).await;

        // Minimum payload — event_name + environment_id + deployment_id
        let payload = serde_json::json!({
            "event_name": "heartbeat",
            "environment_id": environment.id,
            "deployment_id": deployment.id
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/projects/{}/events/ingest", project.id))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let events: Vec<temps_entities::events::Model> = temps_entities::events::Entity::find()
            .filter(temps_entities::events::Column::ProjectId.eq(project.id))
            .all(db.as_ref())
            .await
            .expect("Failed to query events");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_name.as_deref(), Some("heartbeat"));
        // Defaults
        assert_eq!(events[0].pathname, "/");
        assert!(events[0].screen_width.is_none());
        assert!(events[0].user_agent.is_none());
        assert!(events[0].ip_geolocation_id.is_none());

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn test_console_events_appear_in_query_results() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let environment = insert_test_environment(db.as_ref(), project.id).await;
        let deployment = insert_test_deployment(db.as_ref(), project.id, environment.id).await;
        let (_app, state, _crypto) = setup_test_app(db.clone()).await;

        // Ingest 3 events
        for event_name in &["signup", "purchase", "purchase"] {
            let payload = serde_json::json!({
                "event_name": event_name,
                "event_data": {},
                "environment_id": environment.id,
                "deployment_id": deployment.id,
                "request_path": "/api/track"
            });

            let app_clone = configure_routes()
                .layer(middleware::from_fn(
                    |mut req: Request<Body>, next: axum::middleware::Next| async move {
                        req.extensions_mut().insert(create_test_auth_context());
                        next.run(req).await
                    },
                ))
                .with_state(state.clone());

            let response = app_clone
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/projects/{}/events/ingest", project.id))
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_string(&payload).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
        }

        // Verify via has-events endpoint
        let app_query = configure_routes()
            .layer(middleware::from_fn(
                |mut req: Request<Body>, next: axum::middleware::Next| async move {
                    req.extensions_mut().insert(create_test_auth_context());
                    next.run(req).await
                },
            ))
            .with_state(state.clone());

        let response = app_query
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/projects/{}/has-events", project.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let has_events: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(has_events["has_events"], true);

        // Verify all 3 events are in the database
        let events: Vec<temps_entities::events::Model> = temps_entities::events::Entity::find()
            .filter(temps_entities::events::Column::ProjectId.eq(project.id))
            .all(db.as_ref())
            .await
            .expect("Failed to query events");

        assert_eq!(events.len(), 3);

        let event_names: Vec<&str> = events
            .iter()
            .filter_map(|e| e.event_name.as_deref())
            .collect();
        assert_eq!(event_names.iter().filter(|n| **n == "signup").count(), 1);
        assert_eq!(event_names.iter().filter(|n| **n == "purchase").count(), 2);

        test_db.cleanup().await;
    }
}
