use crate::types::requests::{self, *};
use crate::types::responses::*;
use crate::{Analytics, AnalyticsError};
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;
use temps_auth::RequireAuth;
use temps_auth::{
    deny_deployment_token, permission_guard, project_access_guard, project_scope_guard,
};
use temps_core::error_builder::{bad_request, internal_server_error};
use temps_core::problemdetails::Problem;
use temps_core::{not_found, DateTime, UtcDateTime};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

pub struct AppState {
    pub analytics_service: Arc<dyn Analytics>,
    /// Optional checker for team-based project access (human sessions only).
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_analytics_events_count,
        get_event_detail,
        get_event_visitors,
        get_event_entries,
        get_visitors,
        get_visitor_facets,
        get_visitor_details,
        get_visitor_info,
        get_visitor_stats,
        enrich_visitor,
        get_analytics_visitor_sessions,
        get_visitor_journey,
        get_session_details,
        get_analytics_session_events,
        get_session_logs,
        check_analytics_has_events,
        get_page_paths,
        get_page_path_detail,
        get_page_path_visitors,
        get_analytics_active_visitors,
        get_live_visitors_list,
        get_page_hourly_sessions,
        get_page_paths_sparklines,
        get_visitor_by_id,
        get_visitor_by_guid,
        get_general_stats,
        get_page_flow,
        get_recent_activity,
    ),
    components(schemas(
        ViewsOverTime,
        ViewItem,
        PathVisitorsResponse,
        ReferrerCount,
        PathVisitors,
        LocationCount,
        BrowserCount,
        OperatingSystemCount,
        DeviceCount,
        StatusCodeCount,
        EventCount,
        LocationGranularity,
        VisitorsResponse,
        VisitorInfo,
        VisitorDetails,
        VisitorRecord,
        VisitorStats,
        PageVisit,
        LocationInfo,
        VisitorSessionsResponse,
        SessionSummary,
        SessionDetails,
        SessionEvent,
        SessionRequestLog,
        SessionEventsResponse,
        SessionLogsResponse,
        EnrichVisitorRequest,
        EnrichVisitorResponse,
        HasAnalyticsEventsResponse,
        PageSessionStats,
        PagePathInfo,
        PagePathsResponse,
        ActiveVisitor,
        ActiveVisitorsResponse,
        ActiveVisitorsQuery,
        HourlyPageSessions,
        PageHourlySessionsResponse,
        PageSessionComparison,
        PagesComparisonResponse,
        LiveVisitorInfo,
        LiveVisitorsListResponse,
        // Query schemas
        MetricsQuery,
        ViewsOverTimeQuery,
        PathVisitorsAnalyticsQuery,
        ReferrersAnalyticsQuery,
        VisitorLocationsQuery,
        BrowsersQuery,
        StatusCodesQuery,
        EventsCountQuery,
        VisitorsListQuery,
        VisitorSegmentFilters,
        VisitorFacetsQuery,
        VisitorFacets,
        VisitorFacetValue,
        VisitorSessionsQuery,
        SessionDetailsQuery,
        SessionEventsQuery,
        SessionLogsQuery,
        ProjectQuery,
        PageSessionStatsQuery,
        PagePathsQuery,
        PageHourlySessionsQuery,
        PagePathsSparklineQuery,
        PagePathsSparklineResponse,
        PagePathSparkline,
        PagePathSparklinePoint,
        VisitorWithGeolocation,
        EventBreakdown,
        GeneralStatsQuery,
        GeneralStatsResponse,
        ProjectStatsBreakdown,
        // Page path detail types
        PagePathDetailQuery,
        PagePathDetailResponse,
        PagePathVisitorsQuery,
        PagePathVisitorsResponse,
        PageVisitorSession,
        PageActivityBucket,
        PageCountryStats,
        PageReferrerStats,
        VisitorJourneyResponse,
        JourneySession,
        JourneyEvent,
        VisitorJourneyQuery,
        // Page flow / journey types
        PageFlowQuery,
        PageFlowResponse,
        PageFlowEntry,
        PageTransition,
        DropOffPoint,
        // Recent activity types
        RecentActivityQuery,
        RecentActivityResponse,
        ActivityEvent,
        // Event detail types
        EventDetailQuery,
        EventDetailResponse,
        EventActivityBucket,
        EventReferrerStats,
        EventCountryStats,
        EventBrowserStats,
        EventVisitorsQuery,
        EventVisitorsResponse,
        EventVisitorInfo,
        EventEntriesQuery,
        EventEntriesResponse,
        EventEntryInfo,
    )),
    info(
        title = "Analytics API",
        description = "API endpoints for retrieving analytics data including metrics, views, visitors, referrers and more. \
        Provides detailed insights into project usage, visitor behavior, and performance metrics.",
        version = "1.0.0"
    )
)]
pub struct AnalyticsApiDoc;

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/analytics/general-stats", get(get_general_stats))
        .route("/analytics/events", get(get_analytics_events_count))
        .route("/analytics/event-detail", get(get_event_detail))
        .route("/analytics/event-visitors", get(get_event_visitors))
        .route("/analytics/event-entries", get(get_event_entries))
        .route("/analytics/visitors", get(get_visitors))
        .route("/analytics/visitor-facets", get(get_visitor_facets))
        .route("/analytics/visitors/{visitor_id}", get(get_visitor_details))
        .route(
            "/analytics/visitors/{visitor_id}/info",
            get(get_visitor_info),
        )
        .route(
            "/analytics/visitors/{visitor_id}/stats",
            get(get_visitor_stats),
        )
        .route(
            "/analytics/visitors/{visitor_id}/enrich",
            put(enrich_visitor),
        )
        .route(
            "/analytics/visitors/{visitor_id}/sessions",
            get(get_analytics_visitor_sessions),
        )
        .route(
            "/analytics/visitors/{visitor_id}/journey",
            get(get_visitor_journey),
        )
        .route("/analytics/sessions/{session_id}", get(get_session_details))
        .route(
            "/analytics/sessions/{session_id}/events",
            get(get_analytics_session_events),
        )
        .route(
            "/analytics/sessions/{session_id}/logs",
            get(get_session_logs),
        )
        .route("/analytics/has-events", get(check_analytics_has_events))
        .route("/analytics/page-paths", get(get_page_paths))
        .route("/analytics/page-path-detail", get(get_page_path_detail))
        .route("/analytics/page-path-visitors", get(get_page_path_visitors))
        .route(
            "/analytics/active-visitors",
            get(get_analytics_active_visitors),
        )
        .route("/analytics/live-visitors", get(get_live_visitors_list))
        .route(
            "/analytics/page-hourly-sessions",
            get(get_page_hourly_sessions),
        )
        .route(
            "/analytics/page-paths-sparklines",
            get(get_page_paths_sparklines),
        )
        .route("/analytics/visitors/id/{id}", get(get_visitor_by_id))
        .route(
            "/analytics/visitors/guid/{visitor_id}",
            get(get_visitor_by_guid),
        )
        .route("/analytics/page-flow", get(get_page_flow))
        .route("/analytics/recent-activity", get(get_recent_activity))
}

#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/events",
    params(
        ("start_date" = String, Query, description = "Start date in format YYYY-MM-DD HH:MM:SS"),
        ("end_date" = String, Query, description = "End date in format YYYY-MM-DD HH:MM:SS"),
        ("project_id" = i32, Query, description = "Project ID or slug"),
        ("limit" = Option<i32>, Query, description = "Maximum number of results to return"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("custom_events_only" = Option<bool>, Query, description = "Only return custom events, excluding system events like page_view, page_leave, heartbeat (default: true)"),
        ("breakdown" = Option<String>, Query, description = "Breakdown by geography: 'country', 'region', or 'city' (optional)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved event counts", body = Vec<EventCount>),
        (status = 400, description = "Invalid date format, missing required parameters, or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_analytics_events_count(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<EventsCountQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);
    let project_id = query.project_id;

    match app_state
        .analytics_service
        .get_events_count(
            query.start_date.into(),
            query.end_date.into(),
            project_id,
            query.environment_id,
            query.limit,
            query.custom_events_only,
            query.breakdown,
        )
        .await
    {
        Ok(events) => Ok(Json(events)),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get list of visitors with summary information
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/visitors",
    params(
        ("start_date" = String, Query, description = "Start date in format YYYY-MM-DD HH:MM:SS"),
        ("end_date" = String, Query, description = "End date in format YYYY-MM-DD HH:MM:SS"),
        ("project_id" = i32, Query, description = "Project ID or slug"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("include_crawlers" = Option<bool>, Query, description = "Include crawlers (default: false)"),
        ("limit" = Option<i32>, Query, description = "Maximum number of visitors to return (default: 50)"),
        ("offset" = Option<i32>, Query, description = "Number of visitors to skip (default: 0)"),
        ("has_activity_only" = Option<bool>, Query, description = "Filter to only include visitors with recorded activity (events/sessions). When true, excludes ghost visitors (default: true)"),
        // Segment filters — drill into a single visitor-row dimension. All
        // filters resolve against `visitor` + `ip_geolocations` so they stay
        // fast regardless of event volume.
        ("filter_country" = Option<String>, Query, description = "Geolocation country"),
        ("filter_region" = Option<String>, Query, description = "Geolocation region"),
        ("filter_city" = Option<String>, Query, description = "Geolocation city"),
        ("filter_channel" = Option<String>, Query, description = "First-touch channel"),
        ("filter_referrer" = Option<String>, Query, description = "First-touch referrer hostname (use 'Direct' for null)"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved visitors", body = VisitorsResponse),
        (status = 400, description = "Invalid date format, missing required parameters, or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_visitors(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<VisitorsListQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);
    let project_id = query.project_id;

    match app_state
        .analytics_service
        .get_visitors(
            query.start_date.into(),
            query.end_date.into(),
            project_id,
            query.environment_id,
            query.include_crawlers,
            query.limit,
            query.offset,
            Some(query.has_activity_only.unwrap_or(true)),
            query.segment,
        )
        .await
    {
        Ok(visitors) => Ok(Json(visitors)),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get filter dropdown contents for the visitors page. Returns the top
/// values per dimension with distinct visitor counts so the UI can render
/// "Country — 1,234 visitors" rows. Each dimension is computed against the
/// segment minus its own filter, so a selected value never collapses its
/// own dropdown.
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/visitor-facets",
    params(
        ("start_date" = String, Query, description = "Start date in format YYYY-MM-DD HH:MM:SS"),
        ("end_date" = String, Query, description = "End date in format YYYY-MM-DD HH:MM:SS"),
        ("project_id" = i32, Query, description = "Project ID or slug"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("include_crawlers" = Option<bool>, Query, description = "Include crawlers (default: false)"),
        ("has_activity_only" = Option<bool>, Query, description = "Hide ghost visitors (default: true)"),
        ("per_facet_limit" = Option<i32>, Query, description = "Top N values per dimension (default: 50, max: 200)"),
        ("filter_country" = Option<String>, Query, description = "Geolocation country"),
        ("filter_region" = Option<String>, Query, description = "Geolocation region"),
        ("filter_city" = Option<String>, Query, description = "Geolocation city"),
        ("filter_channel" = Option<String>, Query, description = "First-touch channel"),
        ("filter_referrer" = Option<String>, Query, description = "First-touch referrer hostname (use 'Direct' for null)"),
    ),
    responses(
        (status = 200, description = "Top values per dimension", body = VisitorFacets),
        (status = 400, description = "Invalid date format or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_visitor_facets(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<VisitorFacetsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);
    let project_id = query.project_id;

    match app_state
        .analytics_service
        .get_visitor_facets(
            query.start_date.into(),
            query.end_date.into(),
            project_id,
            query.environment_id,
            query.include_crawlers,
            Some(query.has_activity_only.unwrap_or(true)),
            query.per_facet_limit,
            query.segment,
        )
        .await
    {
        Ok(facets) => Ok(Json(facets)),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get detailed information about a specific visitor by numeric ID
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/visitors/{visitor_id}",
    params(
        ("visitor_id" = i32, Path, description = "Visitor numeric ID"),
        ("project_id" = i32, Query, description = "Project ID or slug"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved visitor details", body = VisitorDetails),
        (status = 404, description = "Visitor not found"),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_visitor_details(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(visitor_id): axum::extract::Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    deny_deployment_token!(auth);

    match app_state
        .analytics_service
        .get_visitor_details_by_id(visitor_id)
        .await
    {
        Ok(Some(visitor_details)) => Ok(Json(visitor_details)),
        Ok(None) => Err(bad_request().detail("Visitor not found").build()),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get visitor record from database
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/visitors/{visitor_id}/info",
    params(
        ("visitor_id" = i32, Path, description = "Visitor numeric ID"),
        ("project_id" = i32, Query, description = "Project ID or slug")
    ),
    responses(
        (status = 200, description = "Successfully retrieved visitor info", body = VisitorRecord),
        (status = 404, description = "Visitor not found"),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_visitor_info(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(visitor_id): axum::extract::Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    deny_deployment_token!(auth);

    match app_state
        .analytics_service
        .get_visitor_info(visitor_id)
        .await
    {
        Ok(Some(visitor_info)) => Ok(Json(visitor_info)),
        Ok(None) => Err(bad_request().detail("Visitor not found").build()),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get visitor statistics
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/visitors/{visitor_id}/stats",
    params(
        ("visitor_id" = i32, Path, description = "Visitor numeric ID"),
        ("project_id" = i32, Query, description = "Project ID or slug")
    ),
    responses(
        (status = 200, description = "Successfully retrieved visitor statistics", body = VisitorStats),
        (status = 404, description = "Visitor not found"),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_visitor_stats(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(visitor_id): axum::extract::Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    deny_deployment_token!(auth);

    match app_state
        .analytics_service
        .get_visitor_statistics(visitor_id)
        .await
    {
        Ok(Some(visitor_stats)) => Ok(Json(visitor_stats)),
        Ok(None) => Err(bad_request().detail("Visitor not found").build()),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get all sessions for a specific visitor by numeric ID
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/visitors/{visitor_id}/sessions",
    params(
        ("visitor_id" = i32, Path, description = "Visitor numeric ID"),
        ("project_id" = i32, Query, description = "Project ID or slug"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("limit" = Option<i32>, Query, description = "Maximum number of sessions to return (default: 100)"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved visitor sessions", body = VisitorSessionsResponse),
        (status = 404, description = "Visitor not found"),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_analytics_visitor_sessions(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(visitor_id): axum::extract::Path<i32>,
    Query(query): Query<VisitorSessionsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    deny_deployment_token!(auth);

    match app_state
        .analytics_service
        .get_visitor_sessions_by_id(visitor_id, query.limit)
        .await
    {
        Ok(Some(visitor_sessions)) => Ok(Json(visitor_sessions)),
        Ok(None) => Err(bad_request().detail("Visitor not found").build()),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get the complete visitor journey: all events across all sessions, grouped by session
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/visitors/{visitor_id}/journey",
    params(
        ("visitor_id" = i32, Path, description = "Visitor numeric ID"),
        ("project_id" = i32, Query, description = "Project ID"),
        ("limit_sessions" = Option<i32>, Query, description = "Maximum number of sessions to return (default: 50)"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved visitor journey", body = VisitorJourneyResponse),
        (status = 404, description = "Visitor not found"),
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_visitor_journey(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(visitor_id): axum::extract::Path<i32>,
    Query(query): Query<VisitorJourneyQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    match app_state
        .analytics_service
        .get_visitor_journey(visitor_id, query.project_id, query.limit_sessions)
        .await
    {
        Ok(Some(journey)) => Ok(Json(journey)),
        Ok(None) => Err(bad_request().detail("Visitor not found").build()),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get detailed information about a specific session including events and request logs
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/sessions/{session_id}",
    params(
        ("session_id" = i32, Path, description = "Session ID"),
        ("project_id" = i32, Query, description = "Project ID or slug"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved session details", body = SessionDetails),
        (status = 404, description = "Session not found"),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_session_details(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(session_id): axum::extract::Path<i32>,
    Query(query): Query<SessionDetailsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let project_id = query.project_id;
    match app_state
        .analytics_service
        .get_session_details(session_id, project_id, query.environment_id)
        .await
    {
        Ok(Some(session_details)) => Ok(Json(session_details)),
        Ok(None) => Err(bad_request().detail("Session not found").build()),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/sessions/{session_id}/events",
    params(
        ("session_id" = i32, Path, description = "Session ID"),
        ("project_id" = i32, Query, description = "Project ID or slug"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_date" = Option<String>, Query, description = "Start date in format YYYY-MM-DD HH:MM:SS"),
        ("end_date" = Option<String>, Query, description = "End date in format YYYY-MM-DD HH:MM:SS"),
        ("limit" = Option<i32>, Query, description = "Number of results to return (default: 100)"),
        ("offset" = Option<i32>, Query, description = "Number of results to skip (default: 0)"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved session events", body = SessionEventsResponse),
        (status = 404, description = "Session not found"),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_analytics_session_events(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(session_id): axum::extract::Path<i32>,
    Query(query): Query<SessionEventsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let project_id = query.project_id;

    match app_state
        .analytics_service
        .get_session_events(
            session_id,
            project_id,
            query.environment_id,
            query.start_date.map(|d| d.into()),
            query.end_date.map(|d| d.into()),
            query.limit,
            query.offset,
            query.sort_order,
        )
        .await
    {
        Ok(Some(events_response)) => Ok(Json(events_response)),
        Ok(None) => Err(bad_request()
            .detail("Session not found or access denied")
            .build()),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/sessions/{session_id}/logs",
    params(
        ("session_id" = i32, Path, description = "Session ID"),
        ("project_id" = i32, Query, description = "Project ID or slug"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_date" = Option<String>, Query, description = "Start date in format YYYY-MM-DD HH:MM:SS"),
        ("end_date" = Option<String>, Query, description = "End date in format YYYY-MM-DD HH:MM:SS"),
        ("limit" = Option<i32>, Query, description = "Number of results to return (default: 100)"),
        ("offset" = Option<i32>, Query, description = "Number of results to skip (default: 0)"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved session logs", body = SessionLogsResponse),
        (status = 404, description = "Session not found"),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_session_logs(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(session_id): axum::extract::Path<i32>,
    Query(query): Query<SessionLogsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let project_id = query.project_id;

    match app_state
        .analytics_service
        .get_session_logs(
            session_id,
            project_id,
            query.environment_id,
            query.visitor_id,
            query.start_date.map(|d| d.into()),
            query.end_date.map(|d| d.into()),
            query.limit,
            query.offset,
            query.sort_order,
        )
        .await
    {
        Ok(Some(logs_response)) => Ok(Json(logs_response)),
        Ok(None) => Err(bad_request()
            .detail("Session not found or access denied")
            .build()),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

#[utoipa::path(
    tag = "Analytics",
    put,
    path = "/analytics/visitors/{visitor_id}/enrich",
    params(
        ("visitor_id" = String, Path, description = "Visitor ID - can be numeric ID, GUID, or encrypted GUID (enc_xxx)"),
        ("project_id" = i32, Query, description = "Project ID or slug"),
    ),
    request_body = EnrichVisitorRequest,
    responses(
        (status = 200, description = "Successfully enriched visitor data", body = EnrichVisitorResponse),
        (status = 404, description = "Visitor not found"),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn enrich_visitor(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(visitor_id): axum::extract::Path<String>,
    Json(request): Json<EnrichVisitorRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    deny_deployment_token!(auth);

    // Check if visitor_id is a numeric ID or a GUID/encrypted GUID
    if let Ok(numeric_id) = visitor_id.parse::<i32>() {
        // Numeric ID - use enrich_visitor_by_id
        match app_state
            .analytics_service
            .enrich_visitor_by_id(numeric_id, request.custom_data)
            .await
        {
            Ok(response) => Ok(Json(response)),
            Err(e) => Err(handle_analytics_error(e)),
        }
    } else {
        // GUID or encrypted GUID (enc_xxx) - use enrich_visitor_by_guid
        match app_state
            .analytics_service
            .enrich_visitor_by_guid(&visitor_id, request.custom_data)
            .await
        {
            Ok(response) => Ok(Json(response)),
            Err(e) => Err(handle_analytics_error(e)),
        }
    }
}

#[utoipa::path(
    get,
    path = "/analytics/has-events",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)")
    ),
    responses(
        (status = 200, description = "Analytics events existence check", body = HasAnalyticsEventsResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Project not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Analytics",
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn check_analytics_has_events(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<HasAnalyticsEventsResponse>, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let project_id = query.project_id;
    match app_state
        .analytics_service
        .has_analytics_events(project_id, query.environment_id)
        .await
    {
        Ok(res) => Ok(Json(HasAnalyticsEventsResponse {
            has_events: res.has_events,
        })),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

// Helper function to handle AnalyticsError
pub(super) fn handle_analytics_error(error: AnalyticsError) -> Problem {
    match error {
        AnalyticsError::DatabaseError(e) => {
            tracing::error!("Database error: {}", e);
            internal_server_error()
                .detail("Database error while fetching analytics data")
                .build()
        }
        AnalyticsError::Other(e) => {
            tracing::error!("Other error: {}", e);
            internal_server_error()
                .detail("Failed to fetch analytics data")
                .build()
        }
        AnalyticsError::InvalidVisitorId(visitor_id) => {
            tracing::error!("Invalid visitor ID: {}", visitor_id);
            bad_request()
                .detail(format!("Invalid visitor ID: {}", visitor_id))
                .build()
        }
        AnalyticsError::SessionNotFound(e) => {
            tracing::error!("Session not found: {}", e);
            not_found().detail("Session not found").build()
        }
    }
}

#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/page-paths",
    params(
        ("project_id" = i32, Query, description = "Project ID or slug"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_date" = Option<String>, Query, description = "Start date in format YYYY-MM-DD HH:MM:SS (optional)"),
        ("end_date" = Option<String>, Query, description = "End date in format YYYY-MM-DD HH:MM:SS (optional)"),
        ("limit" = Option<i32>, Query, description = "Maximum number of page paths to return (default: 100, max: 1000)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved page paths", body = PagePathsResponse),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_page_paths(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<PagePathsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    match app_state
        .analytics_service
        .get_page_paths(
            query.project_id,
            query.environment_id,
            query.start_date.map(|d| d.into()),
            query.end_date.map(|d| d.into()),
            query.limit,
        )
        .await
    {
        Ok(page_paths) => {
            let response = PagePathsResponse {
                total_count: page_paths.total_count,
                page_paths: page_paths.page_paths,
            };
            Ok(Json(response))
        }
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get individual visitor sessions for a specific page path
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/page-path-visitors",
    params(
        ("page_path" = String, Query, description = "The page path to get visitors for (URL-encoded)"),
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_date" = String, Query, description = "Start date in ISO 8601 format"),
        ("end_date" = String, Query, description = "End date in ISO 8601 format"),
        ("page" = Option<u64>, Query, description = "Page number (1-based, default: 1)"),
        ("per_page" = Option<u64>, Query, description = "Items per page (default: 50, max: 100)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved page path visitors", body = PagePathVisitorsResponse),
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_page_path_visitors(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<requests::PagePathVisitorsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let start_date: UtcDateTime = query.start_date.into();
    let end_date: UtcDateTime = query.end_date.into();
    let page = query.page.unwrap_or(1);
    let per_page = query.per_page.unwrap_or(50).min(100);

    match app_state
        .analytics_service
        .get_page_path_visitors(
            query.project_id,
            &query.page_path,
            start_date,
            end_date,
            query.environment_id,
            page,
            per_page,
        )
        .await
    {
        Ok(result) => Ok(Json(result)),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get detailed analytics for a specific page path
/// Returns visitors, page views, activity over time, geographic distribution, and referrers
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/page-path-detail",
    params(
        ("page_path" = String, Query, description = "The page path to get details for (URL-encoded)"),
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_date" = String, Query, description = "Start date in ISO 8601 format"),
        ("end_date" = String, Query, description = "End date in ISO 8601 format"),
        ("bucket_interval" = Option<String>, Query, description = "Bucket interval for time series: 'hour', 'day', 'week', 'month' (default: auto based on date range)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved page path detail analytics", body = PagePathDetailResponse),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_page_path_detail(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<requests::PagePathDetailQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let start_date: UtcDateTime = query.start_date.into();
    let end_date: UtcDateTime = query.end_date.into();

    match app_state
        .analytics_service
        .get_page_path_detail(
            query.project_id,
            &query.page_path,
            start_date,
            end_date,
            query.environment_id,
            query.bucket_interval.as_deref(),
        )
        .await
    {
        Ok(detail) => Ok(Json(detail)),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Query parameters for active visitors endpoint
#[derive(Debug, Deserialize, ToSchema, Clone)]
pub struct ActiveVisitorsQuery {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub window_minutes: Option<i32>,
}

/// Get count of active visitors
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/active-visitors/count",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("deployment_id" = Option<i32>, Query, description = "Deployment ID (optional)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved active visitors count", body = i64),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_active_visitors_count(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<ActiveVisitorsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let project_id = query.project_id;
    match app_state
        .analytics_service
        .get_active_visitors_count(project_id, query.environment_id, query.deployment_id)
        .await
    {
        Ok(count) => Ok(Json(count)),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get detailed active visitors
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/active-visitors",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("deployment_id" = Option<i32>, Query, description = "Deployment ID (optional)"),
        ("window_minutes" = Option<i32>, Query, description = "Time window in minutes for active visitors (default: 5)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved active visitors", body = ActiveVisitorsResponse),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_analytics_active_visitors(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<ActiveVisitorsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let project_id = query.project_id;
    let window = query.window_minutes.unwrap_or(5);

    match app_state
        .analytics_service
        .get_active_visitors_details(project_id, query.environment_id, Some(window), None)
        .await
    {
        Ok(visitors) => {
            let count = visitors.visitors.len() as i64;
            let response = ActiveVisitorsResponse {
                count,
                visitors: visitors.visitors,
                window_minutes: window,
            };
            Ok(Json(response))
        }
        Err(e) => {
            error!("Analytics error: {:?}", e);
            Err(handle_analytics_error(e))
        }
    }
}

/// Get list of currently live visitors from visitor table
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/live-visitors",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("window_minutes" = Option<i32>, Query, description = "Time window in minutes for live visitors (default: 5)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved live visitors list", body = LiveVisitorsListResponse),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_live_visitors_list(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<ActiveVisitorsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let project_id = query.project_id;
    let window = query.window_minutes.unwrap_or(5);

    match app_state
        .analytics_service
        .get_live_visitors(project_id, query.environment_id, window)
        .await
    {
        Ok(live_visitors) => {
            let total_count = live_visitors.len() as i64;
            let response = LiveVisitorsListResponse {
                total_count,
                visitors: live_visitors,
                window_minutes: window,
            };
            Ok(Json(response))
        }
        Err(e) => {
            error!("Analytics error: {:?}", e);
            Err(handle_analytics_error(e))
        }
    }
}

/// Query parameters for batch page paths sparkline endpoint
#[derive(Debug, Deserialize, ToSchema, Clone)]
pub struct PagePathsSparklineQuery {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub start_time: DateTime,
    pub end_time: DateTime,
    /// Comma-separated list of page paths
    pub page_paths: String,
}

#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/page-paths-sparklines",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_time" = String, Query, description = "Start time in ISO 8601 format"),
        ("end_time" = String, Query, description = "End time in ISO 8601 format"),
        ("page_paths" = String, Query, description = "Comma-separated list of page paths"),
    ),
    responses(
        (status = 200, description = "Sparkline data for all requested page paths", body = PagePathsSparklineResponse),
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_page_paths_sparklines(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<PagePathsSparklineQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let start_time: UtcDateTime = query.start_time.into();
    let end_time: UtcDateTime = query.end_time.into();

    let page_paths: Vec<String> = query
        .page_paths
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if page_paths.is_empty() {
        return Ok(Json(PagePathsSparklineResponse { sparklines: vec![] }));
    }

    if page_paths.len() > 100 {
        return Err(bad_request()
            .detail("Too many page paths, maximum is 100")
            .build());
    }

    match app_state
        .analytics_service
        .get_page_paths_sparklines(
            query.project_id,
            &page_paths,
            start_time,
            end_time,
            query.environment_id,
        )
        .await
    {
        Ok(res) => Ok(Json(res)),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Query parameters for page hourly sessions endpoint
#[derive(Debug, Deserialize, ToSchema, Clone)]
pub struct PageHourlySessionsQuery {
    pub page_path: String,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub start_time: DateTime,
    pub end_time: DateTime,
    pub bucket_interval: Option<String>, // "hour", "day", "week", "month"
}

#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/page-hourly-sessions",
    params(
        ("page_path" = String, Query, description = "The page path to get sessions for"),
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_time" = String, Query, description = "Start time in format YYYY-MM-DD HH:MM:SS"),
        ("end_time" = String, Query, description = "End time in format YYYY-MM-DD HH:MM:SS"),
        ("bucket_interval" = Option<String>, Query, description = "Bucket interval: 'hour', 'day', 'week', or 'month' (default: auto-determined based on range)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved page sessions with time buckets", body = PageHourlySessionsResponse),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_page_hourly_sessions(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<PageHourlySessionsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let project_id = query.project_id;
    let start_time: UtcDateTime = query.start_time.into();
    let end_time: UtcDateTime = query.end_time.into();

    match app_state
        .analytics_service
        .get_page_hourly_sessions(
            project_id,
            &query.page_path,
            start_time,
            end_time,
            query.environment_id,
        )
        .await
    {
        Ok(res) => {
            let total_sessions = res.hourly_data.iter().map(|h| h.session_count).sum();
            let response = PageHourlySessionsResponse {
                page_path: query.page_path,
                hourly_data: res.hourly_data,
                total_sessions,
                hours: ((end_time - start_time).num_hours() as i32).max(1),
            };
            Ok(Json(response))
        }
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get visitor by numeric ID with geolocation data
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/visitors/id/{id}",
    params(
        ("id" = i32, Path, description = "Visitor numeric ID"),
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved visitor with geolocation", body = VisitorWithGeolocation),
        (status = 404, description = "Visitor not found"),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_visitor_by_id(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    deny_deployment_token!(auth);

    match app_state
        .analytics_service
        .get_visitor_with_geolocation_by_id(id)
        .await
    {
        Ok(Some(visitor)) => Ok(Json(visitor)),
        Ok(None) => Err(bad_request().detail("Visitor not found").build()),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get visitor by GUID with geolocation data
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/visitors/guid/{visitor_id}",
    params(
        ("visitor_id" = String, Path, description = "Visitor GUID (supports enc_ prefix for encrypted IDs)"),
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved visitor with geolocation", body = VisitorWithGeolocation),
        (status = 404, description = "Visitor not found"),
        (status = 400, description = "Invalid parameters or project not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_visitor_by_guid(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(visitor_id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    deny_deployment_token!(auth);

    match app_state
        .analytics_service
        .get_visitor_with_geolocation_by_guid(&visitor_id)
        .await
    {
        Ok(Some(visitor)) => Ok(Json(visitor)),
        Ok(None) => Err(bad_request().detail("Visitor not found").build()),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get general statistics across all projects for a time frame
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/general-stats",
    params(
        ("start_date" = String, Query, description = "Start date in format YYYY-MM-DD HH:MM:SS"),
        ("end_date" = String, Query, description = "End date in format YYYY-MM-DD HH:MM:SS"),
        ("project_ids" = Option<Vec<i32>>, Query, description = "Optional: Filter by specific project IDs (comma-separated)"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("include_project_breakdown" = Option<bool>, Query, description = "Whether to include per-project breakdown (default: false)"),
    ),
    responses(
        (status = 200, description = "Successfully retrieved general statistics", body = GeneralStatsResponse),
        (status = 400, description = "Invalid date format or parameters"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_general_stats(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<GeneralStatsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    deny_deployment_token!(auth);

    match app_state
        .analytics_service
        .get_general_stats(query.start_date.into(), query.end_date.into())
        .await
    {
        Ok(stats) => Ok(Json(stats)),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Get page flow analytics: entry pages, exit pages, drop-off points, and page transitions
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/page-flow",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_date" = String, Query, description = "Start date in ISO 8601 format"),
        ("end_date" = String, Query, description = "End date in ISO 8601 format"),
        ("limit" = Option<i32>, Query, description = "Max entry/exit pages to return (default: 20, max: 100)"),
        ("transitions_limit" = Option<i32>, Query, description = "Max page transitions to return (default: 50, max: 200)"),
        ("min_views_for_dropoff" = Option<i32>, Query, description = "Minimum views for drop-off analysis (default: 5)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved page flow analytics", body = PageFlowResponse),
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_page_flow(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<requests::PageFlowQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let start_date: UtcDateTime = query.start_date.into();
    let end_date: UtcDateTime = query.end_date.into();

    match app_state
        .analytics_service
        .get_page_flow(
            query.project_id,
            start_date,
            end_date,
            query.environment_id,
            query.limit,
            query.transitions_limit,
            query.min_views_for_dropoff,
        )
        .await
    {
        Ok(result) => Ok(Json(result)),
        Err(e) => Err(handle_analytics_error(e)),
    }
}

/// Query parameters for recent activity endpoint
#[derive(Debug, Deserialize, ToSchema, Clone)]
pub struct RecentActivityQuery {
    /// Project ID
    pub project_id: i32,
    /// Environment ID (optional)
    pub environment_id: Option<i32>,
    /// Return events with ID greater than this (for cursor-based polling)
    pub since_id: Option<i64>,
    /// Max number of events to return (default: 50, max: 100)
    pub limit: Option<i32>,
}

/// Get recent activity events for real-time activity feed
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/recent-activity",
    params(
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("since_id" = Option<i64>, Query, description = "Return events with ID greater than this (cursor-based polling)"),
        ("limit" = Option<i32>, Query, description = "Max events to return (default: 50, max: 100)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved recent activity events", body = RecentActivityResponse),
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_recent_activity(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<RecentActivityQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    match app_state
        .analytics_service
        .get_recent_activity(
            query.project_id,
            query.environment_id,
            query.since_id,
            query.limit,
        )
        .await
    {
        Ok(result) => Ok(Json(result)),
        Err(e) => {
            error!("Analytics error: {:?}", e);
            Err(handle_analytics_error(e))
        }
    }
}

/// Get detailed analytics for a specific event
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/event-detail",
    params(
        ("event_name" = String, Query, description = "Event name to get details for"),
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_date" = String, Query, description = "Start date (ISO 8601)"),
        ("end_date" = String, Query, description = "End date (ISO 8601)"),
        ("bucket_interval" = Option<String>, Query, description = "Bucket interval: hour, day, week, month (default: auto)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved event details", body = EventDetailResponse),
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_event_detail(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<requests::EventDetailQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let start_date: UtcDateTime = query.start_date.into();
    let end_date: UtcDateTime = query.end_date.into();

    match app_state
        .analytics_service
        .get_event_detail(
            query.project_id,
            &query.event_name,
            start_date,
            end_date,
            query.environment_id,
            query.bucket_interval.as_deref(),
        )
        .await
    {
        Ok(detail) => Ok(Json(detail)),
        Err(e) => {
            error!("Analytics error: {:?}", e);
            Err(handle_analytics_error(e))
        }
    }
}

/// Get paginated list of visitors who triggered a specific event
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/event-visitors",
    params(
        ("event_name" = String, Query, description = "Event name to list visitors for"),
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_date" = String, Query, description = "Start date (ISO 8601)"),
        ("end_date" = String, Query, description = "End date (ISO 8601)"),
        ("page" = Option<u64>, Query, description = "Page number (1-based, default: 1)"),
        ("per_page" = Option<u64>, Query, description = "Items per page (default: 20, max: 100)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved event visitors", body = EventVisitorsResponse),
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_event_visitors(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<requests::EventVisitorsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let start_date: UtcDateTime = query.start_date.into();
    let end_date: UtcDateTime = query.end_date.into();
    let page = query.page.unwrap_or(1);
    let per_page = query.per_page.unwrap_or(20).min(100);

    match app_state
        .analytics_service
        .get_event_visitors(
            query.project_id,
            &query.event_name,
            start_date,
            end_date,
            query.environment_id,
            page,
            per_page,
        )
        .await
    {
        Ok(result) => Ok(Json(result)),
        Err(e) => {
            error!("Analytics error: {:?}", e);
            Err(handle_analytics_error(e))
        }
    }
}

/// Get paginated list of raw occurrences of a specific event, including custom JSON properties
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/analytics/event-entries",
    params(
        ("event_name" = String, Query, description = "Event name to list occurrences for"),
        ("project_id" = i32, Query, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_date" = String, Query, description = "Start date (ISO 8601)"),
        ("end_date" = String, Query, description = "End date (ISO 8601)"),
        ("page" = Option<u64>, Query, description = "Page number (1-based, default: 1)"),
        ("per_page" = Option<u64>, Query, description = "Items per page (default: 20, max: 100)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved event entries", body = EventEntriesResponse),
        (status = 400, description = "Invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_event_entries(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Query(query): Query<requests::EventEntriesQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, app_state.project_access_checker);

    let start_date: UtcDateTime = query.start_date.into();
    let end_date: UtcDateTime = query.end_date.into();
    let page = query.page.unwrap_or(1);
    let per_page = query.per_page.unwrap_or(20).min(100);

    match app_state
        .analytics_service
        .get_event_entries(
            query.project_id,
            &query.event_name,
            start_date,
            end_date,
            query.environment_id,
            page,
            per_page,
        )
        .await
    {
        Ok(result) => Ok(Json(result)),
        Err(e) => {
            error!("Analytics error: {:?}", e);
            Err(handle_analytics_error(e))
        }
    }
}
