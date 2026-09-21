// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::api_traffic::ApiTrafficService;
use crate::types::api_traffic::*;
use crate::types::requests::{self, *};
use crate::types::responses::*;
use crate::{Analytics, AnalyticsError};
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    routing::{get, put},
    Extension, Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;
use temps_auth::permissions::Permission;
use temps_auth::RequireAuth;
use temps_auth::{
    deny_deployment_token, permission_guard, project_access_guard, project_scope_guard,
};
use temps_core::error_builder::{bad_request, internal_server_error, too_many_requests};
use temps_core::problemdetails::Problem;
use temps_core::{not_found, DateTime, UtcDateTime};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

pub struct AppState {
    pub analytics_service: Arc<dyn Analytics>,
    /// Optional checker for team-based project access (human sessions only).
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    /// API traffic analytics service (proxy_logs GROUP BY queries + optional AI summary).
    pub api_traffic_service: Arc<ApiTrafficService>,
    /// Audit trail for writes made through this module's handlers.
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    /// Caps the visitor-changing enrichments (and so the audit rows) a single
    /// deployment token can create per minute.
    pub enrich_budget: Arc<crate::visitor_audit::EnrichWriteBudget>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_api_timeseries,
        get_api_routes,
        get_api_callers,
        get_api_summary,
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
        // API traffic analytics types
        ApiTimeseriesPoint,
        ApiTimeseriesResponse,
        ApiRouteEntry,
        ApiRoutesResponse,
        ApiCallerEntry,
        ApiCallersResponse,
        ApiTrafficSummary,
        ApiTrafficSummaryResponse,
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
        // API traffic analytics (proxy_logs) -- project-scoped path params
        .route(
            "/projects/{project_id}/api-analytics/timeseries",
            get(get_api_timeseries),
        )
        .route(
            "/projects/{project_id}/api-analytics/routes",
            get(get_api_routes),
        )
        .route(
            "/projects/{project_id}/api-analytics/callers",
            get(get_api_callers),
        )
        .route(
            "/projects/{project_id}/api-analytics/summary",
            get(get_api_summary),
        )
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
            // A real payload is a few identity fields. Reject anything larger
            // before it is buffered and parsed.
            put(enrich_visitor).layer(axum::extract::DefaultBodyLimit::max(ENRICH_MAX_BODY_BYTES)),
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

// ---------------------------------------------------------------------------
// API traffic analytics handlers
// ---------------------------------------------------------------------------

/// Query parameters shared by all API traffic endpoints.
#[derive(Debug, Deserialize, ToSchema, Clone)]
pub struct ApiTrafficQuery {
    /// Filter to a specific environment. When omitted, all environments are
    /// included in the aggregation.
    pub environment_id: Option<i32>,
    /// Window start (ISO 8601). Must precede `end_date`.
    pub start_date: DateTime,
    /// Window end (ISO 8601).
    pub end_date: DateTime,
}

#[derive(Debug, Deserialize, ToSchema, Clone)]
pub struct ApiTrafficSummaryQuery {
    pub environment_id: Option<i32>,
    pub start_date: DateTime,
    pub end_date: DateTime,
    /// Bypass and replace the backend AI-result cache.
    #[serde(default)]
    pub refresh: bool,
}

/// Query parameters for top-routes and top-callers endpoints.
#[derive(Debug, Deserialize, ToSchema, Clone)]
pub struct ApiTrafficLimitQuery {
    pub environment_id: Option<i32>,
    pub start_date: DateTime,
    pub end_date: DateTime,
    /// Maximum rows to return (default: 20, max: 100).
    pub limit: Option<i64>,
    /// Number of ranked rows to skip (default: 0, max: 10,000).
    pub offset: Option<i64>,
}

const MAX_API_TRAFFIC_WINDOW_DAYS: i64 = 31;

fn validate_api_traffic_window(start: UtcDateTime, end: UtcDateTime) -> Result<(), Problem> {
    if start >= end {
        return Err(bad_request()
            .detail("start_date must be earlier than end_date")
            .build());
    }
    if end - start > chrono::Duration::days(MAX_API_TRAFFIC_WINDOW_DAYS) {
        return Err(bad_request()
            .detail(format!(
                "API traffic windows cannot exceed {MAX_API_TRAFFIC_WINDOW_DAYS} days"
            ))
            .build());
    }
    Ok(())
}

/// Return a time-bucketed series of request volume, error rate, and latency
/// percentiles for a project's API traffic.
///
/// Bucket granularity is auto-selected based on the requested window:
/// ≤6 h → 5 min, ≤24 h → 1 h, ≤72 h → 6 h, else → 1 day.
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/projects/{project_id}/api-analytics/timeseries",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_date" = String, Query, description = "Window start (ISO 8601)"),
        ("end_date" = String, Query, description = "Window end (ISO 8601)"),
    ),
    responses(
        (status = 200, description = "Time-series of request volume, errors, and latency", body = ApiTimeseriesResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_api_timeseries(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(project_id): axum::extract::Path<i32>,
    Query(query): Query<ApiTrafficQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let start: UtcDateTime = query.start_date.into();
    let end: UtcDateTime = query.end_date.into();
    validate_api_traffic_window(start, end)?;

    app_state
        .api_traffic_service
        .get_timeseries(project_id, query.environment_id, start, end)
        .await
        .map(Json)
        .map_err(handle_analytics_error)
}

/// Return the top routes (by request count) in a project's API traffic,
/// grouped by raw `(method, path)`.
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/projects/{project_id}/api-analytics/routes",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_date" = String, Query, description = "Window start (ISO 8601)"),
        ("end_date" = String, Query, description = "Window end (ISO 8601)"),
        ("limit" = Option<i64>, Query, description = "Max routes to return (default: 20, max: 100)"),
        ("offset" = Option<i64>, Query, description = "Ranked routes to skip (default: 0)"),
    ),
    responses(
        (status = 200, description = "Top routes by request count", body = ApiRoutesResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_api_routes(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(project_id): axum::extract::Path<i32>,
    Query(query): Query<ApiTrafficLimitQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let start: UtcDateTime = query.start_date.into();
    let end: UtcDateTime = query.end_date.into();
    validate_api_traffic_window(start, end)?;
    let limit = query.limit.unwrap_or(20);
    let offset = query.offset.unwrap_or(0);

    app_state
        .api_traffic_service
        .get_top_routes(project_id, query.environment_id, start, end, limit, offset)
        .await
        .map(Json)
        .map_err(handle_analytics_error)
}

/// Return the top callers (by client IP, ranked by request count) in a
/// project's API traffic window.
///
/// IP addresses are returned as-is from `proxy_logs.client_ip`. The caller
/// is responsible for any presentation-layer masking required by their privacy
/// policy.
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/projects/{project_id}/api-analytics/callers",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_date" = String, Query, description = "Window start (ISO 8601)"),
        ("end_date" = String, Query, description = "Window end (ISO 8601)"),
        ("limit" = Option<i64>, Query, description = "Max callers to return (default: 20, max: 100)"),
        ("offset" = Option<i64>, Query, description = "Ranked callers to skip (default: 0)"),
    ),
    responses(
        (status = 200, description = "Top callers by request count", body = ApiCallersResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_api_callers(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(project_id): axum::extract::Path<i32>,
    Query(query): Query<ApiTrafficLimitQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let start: UtcDateTime = query.start_date.into();
    let end: UtcDateTime = query.end_date.into();
    validate_api_traffic_window(start, end)?;
    let limit = query.limit.unwrap_or(20);
    let offset = query.offset.unwrap_or(0);

    app_state
        .api_traffic_service
        .get_top_callers(project_id, query.environment_id, start, end, limit, offset)
        .await
        .map(Json)
        .map_err(handle_analytics_error)
}

/// Return an AI-generated summary of API traffic for the given window.
///
/// The response always includes `enabled` and `unavailable_reason` so the
/// client can render a meaningful onboarding state even when no AI provider
/// is configured or the project has not opted in. The `summary` field is
/// non-null only when all of the following are true:
///
/// - `projects.ai_api_traffic_summary_enabled = true`
/// - An AI provider is configured and available
/// - The AI call returns parseable JSON within the bounded summary deadline
///
/// This endpoint never returns a 5xx from an AI failure — it always returns
/// 200 with `summary: null` and a human-readable `unavailable_reason`.
#[utoipa::path(
    tag = "Analytics",
    get,
    path = "/projects/{project_id}/api-analytics/summary",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("start_date" = String, Query, description = "Window start (ISO 8601)"),
        ("end_date" = String, Query, description = "Window end (ISO 8601)"),
        ("refresh" = Option<bool>, Query, description = "Bypass and replace the backend AI summary cache"),
    ),
    responses(
        (status = 200, description = "AI traffic summary (summary field may be null when AI is unavailable)", body = ApiTrafficSummaryResponse),
        (status = 400, description = "Bad request"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error (DB errors only; AI failures return 200 with summary: null)")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_api_summary(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    axum::extract::Path(project_id): axum::extract::Path<i32>,
    Query(query): Query<ApiTrafficSummaryQuery>,
) -> Result<impl IntoResponse, Problem> {
    // Summary generation can spend provider credits or launch an authenticated
    // host CLI, so analytics read access alone must never authorize it.
    if !api_summary_permissions_granted(
        auth.has_permission(&Permission::AnalyticsRead),
        auth.has_permission(&Permission::AiGatewayExecute),
    ) {
        return Err(
            temps_core::problemdetails::new(axum::http::StatusCode::FORBIDDEN)
                .with_title("Insufficient Permissions")
                .with_detail("AI traffic summaries require AnalyticsRead and AiGatewayExecute"),
        );
    }
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let start: UtcDateTime = query.start_date.into();
    let end: UtcDateTime = query.end_date.into();
    validate_api_traffic_window(start, end)?;

    app_state
        .api_traffic_service
        .get_summary(project_id, query.environment_id, start, end, query.refresh)
        .await
        .map(Json)
        .map_err(handle_analytics_error)
}

fn api_summary_permissions_granted(analytics_read: bool, ai_execute: bool) -> bool {
    analytics_read && ai_execute
}

// ---------------------------------------------------------------------------
// Existing analytics handlers
// ---------------------------------------------------------------------------

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

/// Request-body ceiling for the enrich route (see `MAX_SCOPED_ENRICHMENT_BYTES`
/// for the tighter limit applied to deployment tokens).
const ENRICH_MAX_BODY_BYTES: usize = 16 * 1024;

/// Extra rules for a deployment token calling the enrich endpoint.
///
/// Tokens (the `TEMPS_API_TOKEN` injected into deployed apps) may enrich, but
/// only a visitor addressed by the sealed `enc_` id from the proxy's visitor
/// cookie: numeric ids and raw GUIDs would let a token pick arbitrary
/// visitors, so they stay limited to user/API-key auth. The payload is bounded
/// too. The token's project confinement is applied in the service.
fn deployment_token_enrich_guard(
    visitor_id: &str,
    custom_data: &serde_json::Value,
) -> Result<(), Problem> {
    if !visitor_id.starts_with("enc_") {
        return Err(temps_core::error_builder::ErrorBuilder::new(
            axum::http::StatusCode::FORBIDDEN,
        )
        .type_("https://temps.sh/probs/deployment-token-not-allowed")
        .title("Encrypted Visitor ID Required")
        .detail(
            "Deployment tokens can only enrich a visitor identified by its \
             encrypted visitor ID (enc_...) from the _temps_visitor_id cookie",
        )
        .permission_denial(
            temps_core::problemdetails::PermissionDenialKind::DeploymentTokenNotAllowed,
            None,
        )
        .build());
    }
    crate::analytics::validate_scoped_enrichment(custom_data).map_err(handle_analytics_error)
}

#[utoipa::path(
    tag = "Analytics",
    put,
    path = "/analytics/visitors/{visitor_id}/enrich",
    params(
        ("visitor_id" = String, Path, description = "Visitor ID - can be numeric ID, GUID, or encrypted GUID (enc_xxx). Deployment tokens (visitors:enrich) may only use the encrypted GUID and only for visitors of their own project."),
    ),
    request_body = EnrichVisitorRequest,
    responses(
        (status = 200, description = "Enrichment result. `success: false` means the visitor was not found (or is not in the caller's project) and nothing was changed.", body = EnrichVisitorResponse),
        (status = 400, description = "Invalid visitor ID, or enrichment data that is not a JSON object within the size limits"),
        (status = 403, description = "Deployment token used with a non-encrypted visitor ID"),
        (status = 429, description = "The deployment token made too many visitor-changing enrichments in the last minute; retry shortly"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
/// Attach attributes (for example the signed-in user's id, name and email) to a
/// visitor.
///
/// The top-level keys of `custom_data` are merged into what is already stored;
/// a key set to `null` is removed. A deployed app can call this with its
/// injected deployment token (permission `visitors:enrich`), using the sealed
/// `enc_…` value of the `_temps_visitor_id` cookie, for visitors of its own
/// project only.
pub async fn enrich_visitor(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    axum::extract::Path(visitor_id): axum::extract::Path<String>,
    Json(request): Json<EnrichVisitorRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsWrite);

    // Enrichment merges top-level keys, so anything but an object is a mistake
    // (and used to be stored verbatim, replacing the visitor's data).
    if !request.custom_data.is_object() {
        return Err(bad_request()
            .title("Invalid Enrichment Data")
            .detail("custom_data must be a JSON object")
            .build());
    }

    // `Some` only for deployment tokens, which are confined to their project.
    let scope_project_id = auth.project_id();
    // For a deployment token: extra rules, then reserve a slot in its write
    // budget before writing. Every change is audited, so the change rate is what
    // bounds the audit trail, and reserving up front (rather than checking, then
    // counting later) keeps concurrent requests from all slipping past the limit.
    // The slot is kept only if this request changes a visitor.
    let reservation = match auth.deployment_token_info() {
        Some(token) => {
            deployment_token_enrich_guard(&visitor_id, &request.custom_data)?;
            match app_state.enrich_budget.try_reserve(token.token_id) {
                Ok(reservation) => Some(reservation),
                Err(refused) => {
                    if refused.first_in_window {
                        // Once per window, so an abused or over-busy token is
                        // visible to the operator without a flood of log lines.
                        tracing::warn!(
                            deployment_token_id = token.token_id,
                            project_id = ?auth.project_id(),
                            "Deployment token exceeded its visitor enrichment budget; \
                             further enrichments are refused until the window resets"
                        );
                    }
                    return Err(too_many_requests()
                        .detail(
                            "This deployment token made too many visitor-changing enrichments in \
                             the last minute; retry shortly",
                        )
                        .build());
                }
            }
        }
        None => None,
    };

    // Names only, bounded: values are routinely personal data and stay out of the
    // audit trail, and key names are caller-chosen so they are truncated. A key
    // sent as `null` is a removal and is recorded as one.
    let (set_keys, removed_keys): (Vec<&String>, Vec<&String>) = {
        let mut set = Vec::new();
        let mut removed = Vec::new();
        for (key, value) in request.custom_data.as_object().into_iter().flatten() {
            if value.is_null() {
                removed.push(key);
            } else {
                set.push(key);
            }
        }
        (set, removed)
    };
    let (custom_data_keys, custom_data_key_count) =
        crate::visitor_audit::bounded_key_names(set_keys.into_iter());
    let (removed_keys, removed_key_count) =
        crate::visitor_audit::bounded_key_names(removed_keys.into_iter());

    // Check if visitor_id is a numeric ID or a GUID/encrypted GUID
    let result = if let Ok(numeric_id) = visitor_id.parse::<i32>() {
        app_state
            .analytics_service
            .enrich_visitor_by_id(numeric_id, scope_project_id, request.custom_data)
            .await
    } else {
        app_state
            .analytics_service
            .enrich_visitor_by_guid(&visitor_id, scope_project_id, request.custom_data)
            .await
    };
    let response = result.map_err(handle_analytics_error)?;

    if response.updated {
        if let Some(reservation) = reservation {
            reservation.commit();
        }
        let token = auth.deployment_token_info();
        let audit = crate::visitor_audit::VisitorEnrichedAudit {
            context: temps_core::AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            actor_kind: if auth.is_deployment_token() {
                "deployment_token"
            } else {
                "user"
            },
            deployment_token_id: token.as_ref().map(|t| t.token_id),
            deployment_token_name: token.map(|t| t.token_name),
            project_id: scope_project_id,
            visitor_row_id: response.visitor_row_id,
            custom_data_keys,
            custom_data_key_count,
            removed_keys,
            removed_key_count,
        };
        // A failed audit write must not fail the enrichment that already happened.
        if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
            error!("Failed to create visitor enrichment audit log: {}", e);
        }
    }

    Ok(Json(response))
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
        AnalyticsError::InvalidEnrichmentData(reason) => bad_request()
            .title("Invalid Enrichment Data")
            .detail(reason)
            .build(),
        AnalyticsError::InvalidVisitorId(visitor_id) => {
            // Client input, not a server fault: keep it out of error-level alerting.
            tracing::debug!("Invalid visitor ID: {}", visitor_id);
            bad_request()
                .detail(format!("Invalid visitor ID: {}", visitor_id))
                .build()
        }
        AnalyticsError::SessionNotFound(e) => {
            tracing::error!("Session not found: {}", e);
            not_found().detail("Session not found").build()
        }
        AnalyticsError::ProjectNotFound(project_id) => {
            tracing::error!(
                project_id,
                "Project not found while fetching analytics data"
            );
            not_found().detail("Project not found").build()
        }
        AnalyticsError::AiRateLimited { reason } => too_many_requests().detail(reason).build(),
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

#[cfg(test)]
mod api_traffic_window_tests {
    use super::*;

    #[test]
    fn rejects_reversed_api_traffic_window() {
        let end: UtcDateTime = chrono::Utc::now();
        let start = end + chrono::Duration::minutes(1);
        assert!(validate_api_traffic_window(start, end).is_err());
    }

    #[test]
    fn rejects_api_traffic_window_beyond_retention_bound() {
        let start: UtcDateTime = chrono::Utc::now();
        let end = start + chrono::Duration::days(MAX_API_TRAFFIC_WINDOW_DAYS + 1);
        assert!(validate_api_traffic_window(start, end).is_err());
    }

    #[test]
    fn accepts_bounded_api_traffic_window() {
        let start: UtcDateTime = chrono::Utc::now();
        let end = start + chrono::Duration::hours(24);
        assert!(validate_api_traffic_window(start, end).is_ok());
    }

    #[test]
    fn ai_summary_requires_both_read_and_execute_permissions() {
        assert!(api_summary_permissions_granted(true, true));
        assert!(!api_summary_permissions_granted(true, false));
        assert!(!api_summary_permissions_granted(false, true));
        assert!(!api_summary_permissions_granted(false, false));
    }
}

#[cfg(test)]
mod enrich_handler_tests {
    use super::*;
    use crate::{cleanup_test_analytics, create_test_analytics_service};
    use axum::http::StatusCode;
    use sea_orm::EntityTrait;
    use serde_json::json;
    use std::sync::Mutex;
    use temps_ai::{AiRequest, AiService};
    use temps_auth::context::AuthContext;
    use temps_core::problemdetails::{PermissionDenialKind, PermissionDenialMarker};
    use temps_entities::deployment_tokens::DeploymentTokenPermission;

    struct UnavailableAi;

    #[async_trait::async_trait]
    impl AiService for UnavailableAi {
        async fn is_available(&self) -> bool {
            false
        }

        async fn complete(
            &self,
            _request: AiRequest,
        ) -> Result<temps_ai::AiResponse, temps_ai::AiError> {
            Err(temps_ai::AiError::NotAvailable)
        }

        async fn chat_stream(
            &self,
            _request: temps_ai::ChatTurnRequest,
        ) -> Result<temps_ai::TokenStream, temps_ai::AiError> {
            Err(temps_ai::AiError::NotAvailable)
        }
    }

    /// Records `(operation_type, user_id, payload)` for every audit write.
    #[derive(Default)]
    struct CapturingAudit(Mutex<Vec<(String, Option<i32>, String)>>);

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for CapturingAudit {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::AuditOperation,
        ) -> anyhow::Result<()> {
            self.0.lock().unwrap().push((
                operation.operation_type(),
                operation.user_id(),
                operation.serialize()?,
            ));
            Ok(())
        }
    }

    fn metadata() -> temps_core::RequestMetadata {
        temps_core::RequestMetadata {
            ip_address: "203.0.113.9".to_string(),
            user_agent: "enrich-test".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        }
    }

    fn token(project_id: i32, permissions: Vec<DeploymentTokenPermission>) -> AuthContext {
        AuthContext::new_deployment_token(
            project_id,
            None,
            None,
            9,
            "app-token".to_string(),
            permissions,
        )
    }

    fn status_of(result: Result<impl IntoResponse, Problem>) -> Option<StatusCode> {
        result.err().map(|problem| problem.into_response().status())
    }

    #[tokio::test]
    async fn deployment_token_enrichment_is_authorized_confined_and_audited() {
        let (service, db, _container) = create_test_analytics_service!(
            "deployment_token_enrichment_is_authorized_confined_and_audited"
        );
        let crypto = temps_core::CookieCrypto::new("test_key_32_bytes_long_for_tests").unwrap();
        let sealed = format!("enc_{}", crypto.encrypt("test_visitor_1").unwrap());
        let visitor = temps_entities::visitor::Entity::find()
            .one(db.as_ref())
            .await
            .unwrap()
            .unwrap();
        let project = visitor.project_id;

        let audit = Arc::new(CapturingAudit::default());
        let state = Arc::new(AppState {
            analytics_service: Arc::new(service),
            project_access_checker: None,
            api_traffic_service: Arc::new(ApiTrafficService::new(
                db.clone(),
                Arc::new(UnavailableAi),
            )),
            audit_service: audit.clone(),
            // Three visitor-changing writes per minute, so the test reaches the limit.
            enrich_budget: Arc::new(crate::visitor_audit::EnrichWriteBudget::new(
                3,
                std::time::Duration::from_secs(60),
            )),
        });
        let call = |auth: AuthContext, id: &str, data: serde_json::Value| {
            let state = state.clone();
            let id = id.to_string();
            async move {
                enrich_visitor(
                    RequireAuth(auth),
                    State(state),
                    Extension(metadata()),
                    axum::extract::Path(id),
                    Json(EnrichVisitorRequest { custom_data: data }),
                )
                .await
                .map(|ok| ok.into_response())
            }
        };
        let stored = || async {
            temps_entities::visitor::Entity::find_by_id(visitor.id)
                .one(db.as_ref())
                .await
                .unwrap()
                .unwrap()
                .custom_data
        };
        let can_enrich = || token(project, vec![DeploymentTokenPermission::VisitorsEnrich]);

        // 403 with the permission-denial marker for a raw GUID and a numeric id.
        for id in ["test_visitor_1".to_string(), visitor.id.to_string()] {
            let denied = call(can_enrich(), &id, json!({"user_id": "u1"}))
                .await
                .expect_err("token must not address a visitor by raw id");
            let response = denied.into_response();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            let marker = response.extensions().get::<PermissionDenialMarker>();
            assert_eq!(
                marker.map(PermissionDenialMarker::kind),
                Some(PermissionDenialKind::DeploymentTokenNotAllowed)
            );
        }
        assert!(stored().await.is_none(), "denied calls must not write");

        // A token without visitors:enrich is refused by the permission guard.
        let no_perm = token(project, vec![DeploymentTokenPermission::FlagsRead]);
        assert_eq!(
            status_of(call(no_perm, &sealed, json!({"user_id": "u1"})).await),
            Some(StatusCode::FORBIDDEN)
        );

        // Oversized and non-object payloads are 400s.
        let big = json!({ "blob": "x".repeat(crate::analytics::MAX_SCOPED_ENRICHMENT_BYTES) });
        assert_eq!(
            status_of(call(can_enrich(), &sealed, big).await),
            Some(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            status_of(call(can_enrich(), &sealed, json!(["not", "an", "object"])).await),
            Some(StatusCode::BAD_REQUEST)
        );

        // A token bound to another project cannot see or change the visitor.
        let other = token(project + 1, vec![DeploymentTokenPermission::VisitorsEnrich]);
        let miss = call(other, &sealed, json!({"user_id": "intruder"}))
            .await
            .expect("cross-project call answers, it does not error");
        assert_eq!(miss.status(), StatusCode::OK);
        assert!(
            stored().await.is_none(),
            "cross-project write must not land"
        );
        assert!(audit.0.lock().unwrap().is_empty());

        // Undecryptable ciphertext is indistinguishable from an unknown visitor.
        let junk = call(can_enrich(), "enc_notarealciphertext", json!({"a": 1}))
            .await
            .expect("undecryptable id must not surface as an error for tokens");
        assert_eq!(junk.status(), StatusCode::OK);

        // The owning project's token writes, and the write is audited by key name.
        let ok = call(
            can_enrich(),
            &sealed,
            json!({"user_id": "u1", "email": "a@b.example"}),
        )
        .await
        .expect("same-project enrichment succeeds");
        assert_eq!(ok.status(), StatusCode::OK);
        let written_body = axum::body::to_bytes(ok.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(
            stored().await,
            Some(json!({"user_id": "u1", "email": "a@b.example"}))
        );
        {
            let log = audit.0.lock().unwrap();
            assert_eq!(log.len(), 1);
            let (operation, actor, payload) = &log[0];
            assert_eq!(operation, "VISITOR_ENRICHED");
            assert_eq!(*actor, None, "a deployment token has no users row");
            assert!(payload.contains("app-token") && payload.contains("\"email\""));
            assert!(
                !payload.contains("a@b.example"),
                "values must not be audited"
            );
        }

        // Re-sending the same identity is a no-op: no rewrite, no second audit row.
        let noop = call(
            can_enrich(),
            &sealed,
            json!({"user_id": "u1", "email": "a@b.example"}),
        )
        .await
        .expect("idempotent enrichment succeeds");
        assert_eq!(audit.0.lock().unwrap().len(), 1);
        let noop_body = axum::body::to_bytes(noop.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(
            written_body, noop_body,
            "a write-only caller must not be able to tell a no-op from a write"
        );

        // Later enrichment merges instead of replacing what is already stored.
        call(can_enrich(), &sealed, json!({"name": "Ada"}))
            .await
            .expect("merge succeeds");
        assert_eq!(
            stored().await,
            Some(json!({"user_id": "u1", "email": "a@b.example", "name": "Ada"}))
        );
        assert_eq!(audit.0.lock().unwrap().len(), 2);

        // A null value removes the key, and the audit entry names it.
        call(can_enrich(), &sealed, json!({"name": null}))
            .await
            .expect("removal succeeds");
        assert_eq!(
            stored().await,
            Some(json!({"user_id": "u1", "email": "a@b.example"}))
        );
        {
            let log = audit.0.lock().unwrap();
            assert_eq!(log.len(), 3);
            assert!(log[2].2.contains("\"custom_data_keys\":[]"));
            assert!(log[2].2.contains("\"removed_keys\":[\"name\"]"));
            assert!(log[2].2.contains("\"removed_key_count\":1"));
        }

        // The token has now made its 3 visitor-changing writes for the window:
        // the next change is refused (429) rather than written unaudited, and no
        // audit row was dropped along the way. No-op repeats did not count.
        let refused = call(can_enrich(), &sealed, json!({"plan": "pro"}))
            .await
            .expect_err("a token over its budget is refused");
        assert_eq!(
            refused.into_response().status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            stored().await,
            Some(json!({"user_id": "u1", "email": "a@b.example"})),
            "a refused enrichment must not write"
        );
        assert_eq!(audit.0.lock().unwrap().len(), 3);
        // Another project's token has its own budget.
        let other_budget = AuthContext::new_deployment_token(
            project + 1,
            None,
            None,
            10,
            "other-app-token".to_string(),
            vec![DeploymentTokenPermission::VisitorsEnrich],
        );
        assert_eq!(
            call(other_budget, &sealed, json!({"plan": "pro"}))
                .await
                .expect("a different token is unaffected")
                .status(),
            StatusCode::OK
        );

        cleanup_test_analytics!(db);
    }
}
