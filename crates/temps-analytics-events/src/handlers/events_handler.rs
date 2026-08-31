// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    extract::{Path, Query, RawQuery, State},
    http::{
        header::{self, HeaderMap, HeaderName},
        Method, StatusCode,
    },
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use std::time::Duration;
use temps_analytics::ingest_keys::{
    extract_analytics_key, resolve_client_identity, resolve_keyed_ingest_scope,
    AnalyticsIngestKeyService, AnalyticsIngestRateLimiter, ANALYTICS_INGEST_KEY_HEADER,
};
use temps_auth::{
    deny_deployment_token, permission_guard, project_access_guard, project_scope_guard, RequireAuth,
};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_proxy::CachedPeerTable;
use tower_http::cors::{Any, CorsLayer};
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
    /// Optional checker for team-based project access (human sessions only).
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    /// ADR-040: resolves an `X-Temps-Analytics-Key` / `?temps_key=` credential
    /// to a project scope for apps Temps does not deploy, where the route table
    /// has no entry to match `Host` against.
    pub ingest_key_service: Arc<AnalyticsIngestKeyService>,
    /// Per-key sliding-window limiter for the keyed ingest path.
    pub ingest_rate_limiter: Arc<AnalyticsIngestRateLimiter>,
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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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
    project_scope_guard!(auth, query.project_id);
    project_access_guard!(auth, query.project_id, state.project_access_checker);

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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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
        ("include_crawlers" = Option<bool>, Query, description = "Include crawler/bot traffic (default: false)"),
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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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
    )
    .with_crawlers(query.include_crawlers.unwrap_or(false));
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
        ("bucket_size" = Option<String>, Query, description = "Time bucket: hour, day, week, month (default: auto-detect)"),
        ("include_crawlers" = Option<bool>, Query, description = "Include crawler/bot traffic (default: false)")
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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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
        include_crawlers: query.include_crawlers.unwrap_or(false),
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
        ("metric" = String, Query, description = "Metric to count: 'sessions' (unique sessions), 'visitors' (unique visitors), 'returning_visitors' (visitors seen before the range), or 'page_views' (total page views) (default: 'sessions')")
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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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

/// Longest BCP-47 tag we will store. Real tags are short ("en", "pt-BR",
/// "zh-Hant-TW"); anything longer is a client sending junk.
const MAX_LANGUAGE_TAG_LEN: usize = 35;

/// Extract the highest-priority language tag from an `Accept-Language` header
/// (or from an SDK-supplied `language` field, which has the same shape).
///
/// Picks the entry with the highest `q` weight rather than simply the first:
/// browsers do send preference order, but the ordering is a convention, not a
/// guarantee, and `de;q=0.1,en;q=0.9` must resolve to `en`. Ties keep the
/// earlier entry, matching RFC 9110.
///
/// The result is **validated and normalised, not merely truncated**. Every
/// source of this value is attacker-controlled (`/_temps/event` is
/// unauthenticated) and `language` is a `GROUP BY` dimension, so unvalidated
/// input is an unbounded-cardinality hazard — worse on ClickHouse, where the
/// column is `LowCardinality(String)`. Anything that isn't a plausible BCP-47
/// tag is dropped rather than stored, and casing is normalised so `en-US`,
/// `en-us` and `EN-US` collapse to one dimension value instead of three.
fn primary_accept_language(header: &str) -> Option<String> {
    let mut best: Option<(f32, &str)> = None;

    for entry in header.split(',') {
        let mut parts = entry.split(';');
        let tag = parts.next().unwrap_or("").trim().trim_matches('"');
        if tag.is_empty() {
            continue;
        }
        // q-weight defaults to 1.0 when absent or unparsable.
        let q = parts
            .find_map(|p| {
                let p = p.trim();
                p.strip_prefix("q=").or_else(|| p.strip_prefix("Q="))
            })
            .and_then(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(1.0);

        if best.is_none_or(|(best_q, _)| q > best_q) {
            best = Some((q, tag));
        }
    }

    let tag = best.map(|(_, t)| t)?;

    if tag.len() > MAX_LANGUAGE_TAG_LEN {
        return None;
    }
    // "*" is a valid Accept-Language value but carries no information.
    if tag == "*" {
        return None;
    }
    // Subtags are alphanumeric, separated by "-". Reject anything else.
    if !tag
        .split('-')
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric()))
    {
        return None;
    }

    Some(tag.to_ascii_lowercase())
}

/// Record analytics event
#[utoipa::path(
    tag = "Metrics",
    post,
    path = "/_temps/event",
    request_body = EventMetricsPayload,
    params(
        ("x-temps-analytics-key" = Option<String>, Header, description = "Analytics ingest key (ADR-040), `pa_` followed by 64 hex characters. An alternative to Host-based project resolution, for apps Temps does not deploy and which therefore have no route-table entry. When present it takes precedence and the Host header is not consulted for resolution; a key that does not resolve to an active row is a 401, never a fallback to Host. The value is public by design — it ships in client JS — and is write-only: it grants analytics ingest for one project (optionally one environment) and nothing else."),
        ("temps_key" = Option<String>, Query, description = "Query-string fallback for the analytics ingest key, for clients that cannot set custom headers (`navigator.sendBeacon`, used for page-unload events). Consulted only when the `x-temps-analytics-key` header is absent; identical precedence and error semantics.")
    ),
    responses(
        (status = 204, description = "Event recorded successfully"),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error")
    )
)]
pub async fn record_event_metrics(
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    RawQuery(raw_query): RawQuery,
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

    // ADR-040 §3. A presented ingest key resolves the scope outright; `Host` is
    // never consulted for resolution in that branch, and an unresolvable key is
    // a 401 rather than a silent fall-through to `Host` (which would either
    // mis-attribute a typo'd key's data or 404 confusingly).
    //
    // `site_hostname` is resolved alongside the scope, because what the Host
    // header *means* differs per branch. On the Host-resolved branch it is the
    // tracked site itself. On the keyed branch the request terminates at the
    // Temps server, so `metadata.host` is *Temps'* own hostname and has nothing
    // to do with the customer's site — passing it through would make every
    // keyed event look like it happened on the analytics backend, and
    // `get_channel` would classify the customer's real internal navigation as
    // "Referral" instead of a self-referral.
    let (project_id, environment_id, deployment_id, site_hostname, is_keyed) = if let Some(key) =
        extract_analytics_key(&headers, raw_query.as_deref())
    {
        let origin = headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok());
        match resolve_keyed_ingest_scope(
            state.ingest_key_service.as_ref(),
            state.ingest_rate_limiter.as_ref(),
            &key,
            origin,
        )
        .await
        {
            Ok(scope) => {
                info!(
                    "Resolved analytics ingest key {} to project={}, env={:?}, deploy={:?}",
                    scope.key_id, scope.project_id, scope.environment_id, scope.deployment_id
                );
                // No site hostname from this branch — see above. The service
                // falls back to what the SDK put in the payload.
                (
                    scope.project_id,
                    scope.environment_id,
                    scope.deployment_id,
                    None,
                    true,
                )
            }
            Err(problem) => return problem.into_response(),
        }
    } else {
        if host.is_empty() {
            error!("Missing Host header");
            return StatusCode::BAD_REQUEST.into_response();
        }

        // Look up project/environment/deployment from route table o(1)
        match state.route_table.get_route(&host) {
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

                // Unchanged: on this branch the Host header *is* the tracked
                // site, so it stays the site hostname exactly as before.
                (
                    project_id,
                    environment_id,
                    deployment_id,
                    (!host.is_empty()).then(|| host.clone()),
                    false,
                )
            }
            None => {
                error!("Host {} not found in route table", host);
                // Return 404 or BAD_REQUEST since we can't track events for unknown hosts
                return StatusCode::NOT_FOUND.into_response();
            }
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

    // Resolve the visitor's language from the payload, then event_data, then
    // the Accept-Language header — and validate whichever one wins.
    //
    // The header fallback exists because older SDKs send no `language` at all,
    // which left this column NULL on every event and the breakdown 100%
    // "Unknown". Newer SDKs do send it on the payload.
    //
    // Validation is applied to the RESOLVED value rather than to any single
    // branch. All three sources are attacker-controlled — `/_temps/event` is
    // unauthenticated, so `payload.language` and `event_data.language` are just
    // as forgeable as the header — and `language` is a GROUP BY dimension, so
    // an unvalidated value here is an unbounded-cardinality hazard on a box we
    // expect to run in 4 GB. Validating only the header would have left the
    // guard covering the branch least likely to be taken.
    let language = payload
        .language
        .or_else(|| {
            payload
                .event_data
                .get("language")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            headers
                .get("accept-language")
                .and_then(|h| h.to_str().ok())
                .map(|h| h.to_string())
        })
        .and_then(|raw| primary_accept_language(&raw));

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

    let session_id = resolve_client_identity(
        metadata.session_id_cookie,
        payload.session_id.clone(),
        is_keyed,
    );
    let visitor_id = resolve_client_identity(
        metadata.visitor_id_cookie,
        payload.visitor_id.clone(),
        is_keyed,
    );

    // The SDK sends `domain` as a sibling of `event_data`, not nested inside
    // it, but `resolve_site_hostname`'s data-derived fallback (ADR-040 §3)
    // only looks inside `event_data`. Fold it in here rather than widening
    // that function's signature, so the fallback works for real browser
    // traffic on the keyed path instead of only in tests that construct
    // `event_data.domain` directly.
    let mut event_data = payload.event_data;
    if let (Some(domain), serde_json::Value::Object(map)) = (&payload.domain, &mut event_data) {
        map.entry("domain")
            .or_insert_with(|| serde_json::Value::String(domain.clone()));
    }

    match state
        .events_writer
        .record_event(
            project_id,
            environment_id,
            deployment_id,
            session_id,
            visitor_id,
            &payload.event_name,
            event_data,
            &payload.request_path,
            &payload.request_query,
            // The tracked site's own host — lets the service detect
            // self-referrals and attribute channels. `None` on the keyed path,
            // where the Host header names the Temps server rather than the
            // customer's site (ADR-040 §3).
            site_hostname.as_deref(),
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
            // Once-per-instance: this event means "analytics is in use on this
            // instance", not "an analytics event arrived". Guard it so it fires
            // once in the instance's lifetime, not on every pageview.
            state.telemetry.report_once(
                "analytics_first_event_received",
                temps_core::telemetry::TelemetryEvent::new(
                    temps_core::telemetry::TelemetryEventKind::AnalyticsFirstEventReceived,
                ),
            );
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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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
            // No site hostname on the server-side path: the caller is the app
            // backend, not the browser, and it sends no referrer either — so
            // there is no self-referral to detect.
            None, // site_hostname
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
    // Once-per-instance (see record_event): guard so this fires once, not on
    // every console-ingested event.
    state.telemetry.report_once(
        "analytics_first_event_received",
        temps_core::telemetry::TelemetryEvent::new(
            temps_core::telemetry::TelemetryEventKind::AnalyticsFirstEventReceived,
        ),
    );

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
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

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
    // This batch endpoint accepts an arbitrary list of project_ids, so there is
    // no single project to scope a deployment token against. A project-bound
    // machine credential has no business querying cross-project dashboards;
    // require a real user / API-key session.
    deny_deployment_token!(auth);

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
/// without authentication — the project is resolved from the Host header, or
/// from an `X-Temps-Analytics-Key` / `?temps_key=` ingest key (ADR-040).
pub fn configure_public_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/_temps/event", post(record_event_metrics))
        .layer(public_ingest_cors())
}

/// CORS for the public analytics ingest routes.
///
/// Required by ADR-040: with a key, the request is cross-origin by definition,
/// and without this layer the browser blocks it before it ever leaves the page.
///
/// `allow_credentials` stays at its default `false`, and must never be set
/// true. The entire point of key-based ingest is that it needs no cookies;
/// credentialed CORS on a wildcard origin would be a real vulnerability, and
/// the browser rejects the combination outright.
pub(crate) fn public_ingest_cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            HeaderName::from_static(ANALYTICS_INGEST_KEY_HEADER),
        ])
        .max_age(Duration::from_secs(600))
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

    #[test]
    fn test_primary_accept_language_takes_highest_priority_tag() {
        // Browsers send preference order already, so the first entry wins and
        // the q-weight is dropped.
        assert_eq!(
            primary_accept_language("en-US,en;q=0.9,es;q=0.8").as_deref(),
            Some("en-us")
        );
        // Highest q wins even when it is not first — browser ordering is a
        // convention, not a guarantee.
        assert_eq!(
            primary_accept_language("de;q=0.1,en;q=0.9").as_deref(),
            Some("en")
        );
        // Ties keep the earlier entry (RFC 9110).
        assert_eq!(primary_accept_language("fr,de").as_deref(), Some("fr"));
        // Casing is normalised so one language is one dimension value.
        assert_eq!(primary_accept_language("EN-US").as_deref(), Some("en-us"));
        assert_eq!(
            primary_accept_language("en-us").as_deref(),
            primary_accept_language("EN-us").as_deref()
        );
        assert_eq!(primary_accept_language("fr").as_deref(), Some("fr"));
        assert_eq!(
            primary_accept_language("  pt-BR ;q=1.0 ").as_deref(),
            Some("pt-br")
        );
        assert_eq!(
            primary_accept_language("zh-Hant-TW,zh;q=0.9").as_deref(),
            Some("zh-hant-tw")
        );
    }

    #[test]
    fn test_primary_accept_language_rejects_junk() {
        // `language` is a GROUP BY dimension — unvalidated input is a
        // cardinality bomb, so anything implausible is dropped, not stored.
        assert_eq!(primary_accept_language(""), None);
        assert_eq!(primary_accept_language("   "), None);
        assert_eq!(primary_accept_language("*"), None);
        assert_eq!(primary_accept_language(";q=0.9"), None);
        assert_eq!(primary_accept_language("en_US"), None); // underscore, not BCP-47
        assert_eq!(primary_accept_language("en-"), None); // empty subtag
        assert_eq!(primary_accept_language("<script>"), None);
        assert_eq!(primary_accept_language("'; DROP TABLE events--"), None);
        assert_eq!(primary_accept_language(&"a".repeat(36)), None); // over the cap
        assert_eq!(
            primary_accept_language(&"a".repeat(35)).as_deref(),
            Some("a".repeat(35).as_str())
        );
    }

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
            must_change_password: false,
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
            project_access_checker: None,
            ingest_key_service: Arc::new(AnalyticsIngestKeyService::new(db.clone())),
            ingest_rate_limiter: Arc::new(AnalyticsIngestRateLimiter::new()),
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
            error_source_context_enabled: Set(false),
            error_source_root: Set(None),
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

    /// Build an app whose auth middleware injects a deployment token bound to
    /// `token_project_id` with FullAccess — the worst case for cross-tenant IDOR.
    fn setup_deployment_token_app(state: Arc<AppState>, token_project_id: i32) -> axum::Router {
        let auth_middleware = middleware::from_fn(
            move |mut req: Request<Body>, next: axum::middleware::Next| async move {
                let auth_context = temps_auth::AuthContext::new_deployment_token(
                    token_project_id,
                    None,
                    None,
                    1,
                    "test-deployment-token".to_string(),
                    vec![temps_entities::deployment_tokens::DeploymentTokenPermission::FullAccess],
                );
                req.extensions_mut().insert(auth_context);
                next.run(req).await
            },
        );

        configure_routes().layer(auth_middleware).with_state(state)
    }

    #[tokio::test]
    async fn test_full_access_deployment_token_cannot_read_other_project_analytics() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let (_app, state, _crypto) = setup_test_app(db.clone()).await;

        // Token is bound to a DIFFERENT project than the one in the path.
        let other_project_id = project.id + 1;
        let app = setup_deployment_token_app(state.clone(), other_project_id);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/projects/{}/has-events", project.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Even with FullAccess, the token must be denied cross-project access.
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a FullAccess deployment token bound to project {} must not read project {}",
            other_project_id,
            project.id
        );

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn test_deployment_token_can_read_its_own_project_analytics() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let (_app, state, _crypto) = setup_test_app(db.clone()).await;

        // Token bound to the SAME project as the path — should be allowed.
        let app = setup_deployment_token_app(state.clone(), project.id);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/projects/{}/has-events", project.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a deployment token bound to project {} must be able to read its own analytics",
            project.id
        );

        test_db.cleanup().await;
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
            project_access_checker: None,
            ingest_key_service: Arc::new(AnalyticsIngestKeyService::new(db.clone())),
            ingest_rate_limiter: Arc::new(AnalyticsIngestRateLimiter::new()),
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

    // ── ADR-040: keyed ingest on POST /_temps/event ──────────────────────
    //
    // Two things are under test here, and the second matters more than the
    // first: that a key resolves the scope without the route table, and that
    // the *no-key* path is byte-for-byte what it was before keys existed.
    // Breaking a Temps-deployed app's analytics would be worse than not
    // shipping keyed ingest at all.

    /// A syntactically valid key that is guaranteed not to exist.
    const UNKNOWN_KEY: &str = "pa_0000000000000000000000000000000000000000000000000000000000000000";

    fn test_request_metadata(headers: &axum::http::HeaderMap) -> temps_core::RequestMetadata {
        let host = headers
            .get(axum::http::header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        temps_core::RequestMetadata {
            ip_address: String::new(),
            user_agent: String::new(),
            headers: headers.clone(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: format!("http://{host}"),
            scheme: "http".to_string(),
            host,
            is_secure: false,
        }
    }

    /// The public ingest router with a middleware that fabricates the
    /// `RequestMetadata` the real server injects, deriving `host` from the
    /// request's own `Host` header so tests can exercise both branches.
    fn setup_public_app(state: Arc<AppState>) -> axum::Router {
        let metadata_middleware = middleware::from_fn(
            |mut req: Request<Body>, next: axum::middleware::Next| async move {
                let metadata = test_request_metadata(req.headers());
                req.extensions_mut().insert(metadata);
                next.run(req).await
            },
        );

        configure_public_routes()
            .layer(metadata_middleware)
            .with_state(state)
    }

    fn event_payload() -> serde_json::Value {
        serde_json::json!({
            "event_name": "page_view",
            "event_data": {},
            "request_path": "/pricing",
            "request_query": ""
        })
    }

    async fn stored_events(db: &sea_orm::DatabaseConnection) -> Vec<temps_entities::events::Model> {
        temps_entities::events::Entity::find()
            .all(db)
            .await
            .expect("Failed to query events")
    }

    /// Register a host in the route table so the no-key branch can resolve it,
    /// without needing a real deployment to exist.
    fn insert_test_route(
        route_table: &temps_proxy::CachedPeerTable,
        host: &str,
        project: Option<projects::Model>,
    ) {
        route_table.insert_route_for_test(
            host,
            temps_routes::RouteInfo {
                backend: temps_routes::BackendType::StaticDir {
                    path: "/tmp".to_string(),
                },
                redirect_to: None,
                status_code: None,
                project: project.map(Arc::new),
                environment: None,
                deployment: None,
                cert_eligible: false,
            },
        );
    }

    #[tokio::test]
    async fn keyed_event_ingest_resolves_the_keys_project_without_the_route_table() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let (_app, state, _crypto) = setup_test_app(db.clone()).await;

        let key = state
            .ingest_key_service
            .create(project.id, None, None, None, None, None)
            .await
            .expect("minting an ingest key must succeed");

        // Deliberately a host the route table has never heard of — this is the
        // whole point of ADR-040.
        let response = setup_public_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/event")
                    .header("host", "app.not-deployed-by-temps.test")
                    .header("content-type", "application/json")
                    .header("x-temps-analytics-key", &key.public_key)
                    .body(Body::from(event_payload().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let events = stored_events(db.as_ref()).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].project_id, project.id);
        assert_eq!(events[0].environment_id, None);
        assert_eq!(events[0].deployment_id, None);
        // On the keyed path the request terminates at Temps, so the Host header
        // is *Temps'* hostname, not the customer's site. It must not be
        // inherited as the event's site hostname: doing so would break
        // self-referral detection and misreport every keyed event's origin.
        assert_ne!(
            events[0].hostname, "app.not-deployed-by-temps.test",
            "a keyed event must not inherit the Temps server's own hostname"
        );

        test_db.cleanup().await;
    }

    /// The site's real domain reaches the stored event through the payload, not
    /// through `Host`, on the keyed path.
    #[tokio::test]
    async fn keyed_event_ingest_takes_its_hostname_from_the_payload_domain() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let (_app, state, _crypto) = setup_test_app(db.clone()).await;

        let key = state
            .ingest_key_service
            .create(project.id, None, None, None, None, None)
            .await
            .expect("minting an ingest key must succeed");

        let mut payload = event_payload();
        payload["event_data"] = serde_json::json!({ "domain": "shop.example.com" });

        let response = setup_public_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/event")
                    // The Temps server's own hostname, as the browser would
                    // send it on a cross-origin POST.
                    .header("host", "analytics.temps.test")
                    .header("content-type", "application/json")
                    .header("x-temps-analytics-key", &key.public_key)
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let events = stored_events(db.as_ref()).await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].hostname, "shop.example.com",
            "the customer's own domain must win over the Temps host"
        );

        test_db.cleanup().await;
    }

    /// The SDK actually sends `domain` as a sibling of `event_data`, not
    /// nested inside it (see `Analytics.ts`'s request body construction) —
    /// this is the shape real browser traffic uses, unlike the previous test.
    #[tokio::test]
    async fn keyed_event_ingest_takes_its_hostname_from_the_top_level_domain_field() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let (_app, state, _crypto) = setup_test_app(db.clone()).await;

        let key = state
            .ingest_key_service
            .create(project.id, None, None, None, None, None)
            .await
            .expect("minting an ingest key must succeed");

        let mut payload = event_payload();
        payload["domain"] = serde_json::json!("shop.example.com");

        let response = setup_public_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/event")
                    .header("host", "analytics.temps.test")
                    .header("content-type", "application/json")
                    .header("x-temps-analytics-key", &key.public_key)
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let events = stored_events(db.as_ref()).await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].hostname, "shop.example.com",
            "the top-level `domain` field the real SDK sends must be folded \
             into event_data so the site's own hostname is stored, not \
             \"localhost\""
        );

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn keyed_event_ingest_accepts_the_query_param_fallback() {
        // `navigator.sendBeacon` cannot set headers, so `?temps_key=` is the
        // only way the page-unload event can authenticate.
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let (_app, state, _crypto) = setup_test_app(db.clone()).await;

        let key = state
            .ingest_key_service
            .create(project.id, None, None, None, None, None)
            .await
            .expect("minting an ingest key must succeed");

        let response = setup_public_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/_temps/event?temps_key={}", key.public_key))
                    .header("host", "app.not-deployed-by-temps.test")
                    .header("content-type", "application/json")
                    .body(Body::from(event_payload().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(stored_events(db.as_ref()).await.len(), 1);

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn invalid_event_key_returns_401_and_never_falls_back_to_host() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let (_app, state, _crypto) = setup_test_app(db.clone()).await;

        // The Host *would* resolve. A typo'd key must still be a loud 401
        // rather than silently mis-attributed data.
        insert_test_route(
            &state.route_table,
            "app.example.test",
            Some(project.clone()),
        );

        let response = setup_public_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/event")
                    .header("host", "app.example.test")
                    .header("content-type", "application/json")
                    .header("x-temps-analytics-key", UNKNOWN_KEY)
                    .body(Body::from(event_payload().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            stored_events(db.as_ref()).await.is_empty(),
            "a rejected key must not fall through to Host-based resolution"
        );

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn malformed_event_key_returns_401() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let (_app, state, _crypto) = setup_test_app(db.clone()).await;
        insert_test_route(&state.route_table, "app.example.test", Some(project));

        // A `tk_` admin API key pasted into the analytics header must never be
        // accepted, and must never be looked up against `api_keys` either.
        let response = setup_public_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/event")
                    .header("host", "app.example.test")
                    .header("content-type", "application/json")
                    .header("x-temps-analytics-key", "tk_not_an_analytics_key")
                    .body(Body::from(event_payload().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(stored_events(db.as_ref()).await.is_empty());

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn scoped_event_key_enforces_the_origin_allowlist() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let (_app, state, _crypto) = setup_test_app(db.clone()).await;

        let key = state
            .ingest_key_service
            .create(
                project.id,
                None,
                None,
                Some(vec!["https://app.example.com".to_string()]),
                None,
                None,
            )
            .await
            .expect("minting an ingest key must succeed");

        let post = |origin: Option<&'static str>| {
            let app = setup_public_app(state.clone());
            let public_key = key.public_key.clone();
            async move {
                let mut builder = Request::builder()
                    .method("POST")
                    .uri("/_temps/event")
                    .header("host", "app.example.com")
                    .header("content-type", "application/json")
                    .header("x-temps-analytics-key", public_key);
                if let Some(origin) = origin {
                    builder = builder.header("origin", origin);
                }
                app.oneshot(
                    builder
                        .body(Body::from(event_payload().to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
            }
        };

        // No Origin at all against a non-empty allowlist.
        assert_eq!(post(None).await, StatusCode::FORBIDDEN);
        // Wrong origin.
        assert_eq!(
            post(Some("https://evil.example.com")).await,
            StatusCode::FORBIDDEN
        );
        assert!(
            stored_events(db.as_ref()).await.is_empty(),
            "rejected origins must not store an event"
        );

        // Matching origin passes.
        assert_eq!(
            post(Some("https://app.example.com")).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(stored_events(db.as_ref()).await.len(), 1);

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn event_key_over_its_rate_limit_returns_429() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let (_app, state, _crypto) = setup_test_app(db.clone()).await;

        let key = state
            .ingest_key_service
            .create(project.id, None, None, None, Some(1), None)
            .await
            .expect("minting an ingest key must succeed");

        let post = || {
            let app = setup_public_app(state.clone());
            let public_key = key.public_key.clone();
            async move {
                app.oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/_temps/event")
                        .header("host", "app.example.com")
                        .header("content-type", "application/json")
                        .header("x-temps-analytics-key", public_key)
                        .body(Body::from(event_payload().to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
            }
        };

        assert_eq!(post().await, StatusCode::NO_CONTENT);
        assert_eq!(post().await, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(stored_events(db.as_ref()).await.len(), 1);

        test_db.cleanup().await;
    }

    /// Regression: no key at all must behave exactly as it did before ADR-040 —
    /// resolve from Host, record the event with that project.
    #[tokio::test]
    async fn no_key_still_resolves_the_event_scope_from_host() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_test_project(db.as_ref()).await;
        let (_app, state, _crypto) = setup_test_app(db.clone()).await;
        insert_test_route(
            &state.route_table,
            "app.example.test",
            Some(project.clone()),
        );

        let response = setup_public_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/event")
                    .header("host", "app.example.test")
                    .header("content-type", "application/json")
                    .body(Body::from(event_payload().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let events = stored_events(db.as_ref()).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].project_id, project.id);
        assert_eq!(events[0].hostname, "app.example.test");

        test_db.cleanup().await;
    }

    /// Regression: the three no-key rejection shapes are unchanged — empty Host
    /// is 400, an unknown host is 404, and a sandbox/orphan route silently
    /// drops with 204.
    #[tokio::test]
    async fn no_key_rejection_shapes_are_unchanged() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let (_app, state, _crypto) = setup_test_app(db.clone()).await;
        insert_test_route(&state.route_table, "orphan.example.test", None);

        // Empty Host -> 400.
        let response = setup_public_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/event")
                    .header("host", "")
                    .header("content-type", "application/json")
                    .body(Body::from(event_payload().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // Unknown host -> 404.
        let response = setup_public_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/event")
                    .header("host", "unknown.example.test")
                    .header("content-type", "application/json")
                    .body(Body::from(event_payload().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // Route without a project -> 204, silently dropped.
        let response = setup_public_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/event")
                    .header("host", "orphan.example.test")
                    .header("content-type", "application/json")
                    .body(Body::from(event_payload().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        assert!(stored_events(db.as_ref()).await.is_empty());

        test_db.cleanup().await;
    }

    /// The preflight the browser sends before a cross-origin keyed POST must
    /// succeed, and must not advertise credentialed CORS.
    #[tokio::test]
    async fn public_ingest_answers_cors_preflight_without_credentials() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let (_app, state, _crypto) = setup_test_app(db.clone()).await;

        let response = setup_public_app(state)
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/_temps/event")
                    .header("host", "app.example.com")
                    .header("origin", "https://app.example.com")
                    .header("access-control-request-method", "POST")
                    .header("access-control-request-headers", "x-temps-analytics-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(response.status().is_success(), "{}", response.status());
        let headers = response.headers();
        assert_eq!(
            headers
                .get("access-control-allow-origin")
                .and_then(|v| v.to_str().ok()),
            Some("*")
        );
        assert!(
            headers.get("access-control-allow-credentials").is_none(),
            "wildcard-origin ingest must never be credentialed"
        );

        test_db.cleanup().await;
    }
}
