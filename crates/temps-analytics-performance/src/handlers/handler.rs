// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::types::AppState;
use crate::services::service::{
    GroupBy, GroupedPageMetric, GroupedPageMetricsResponse, MetricsOverTimeResponse,
    PerformanceMetricsResponse, RecordPerformanceMetricsConfig, SpeedSegmentFilters,
    UpdatePerformanceMetricsConfig,
};
use axum::http::header::{self, HeaderMap, HeaderName};
use axum::Extension;
use axum::{
    extract::{Query, RawQuery, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use temps_analytics::ingest_keys::{
    extract_analytics_key, resolve_client_identity, resolve_keyed_ingest_scope,
    ANALYTICS_INGEST_KEY_HEADER,
};
use temps_auth::{project_access_guard, AuthContext, Permission, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::DateTime;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info};
use utoipa::{OpenApi, ToSchema};

/// Manual auth check for this crate's dashboard-query handlers.
///
/// These handlers predate the `Result<impl IntoResponse, Problem>` convention
/// used elsewhere and return `(StatusCode, Json<ErrorResponse>)` instead, so
/// they can't use the `permission_guard!`/`project_scope_guard!` macros
/// directly (those return `Problem`). This mirrors the same two checks, plus
/// the team-membership project access check via `project_access_guard!`
/// (bridged to `Problem` internally by `require_project_access` and then
/// converted back to this crate's `ErrorResponse` shape).
async fn require_analytics_read(
    auth: &AuthContext,
    project_id: i32,
    project_access_checker: &Option<Arc<dyn temps_core::ProjectAccessChecker>>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if !auth.has_permission(&Permission::AnalyticsRead) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                error: "Insufficient permissions".to_string(),
                details: Some("This operation requires the AnalyticsRead permission".to_string()),
            }),
        ));
    }
    if !auth.is_scoped_to_project(project_id) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                error: "Cross-project access denied".to_string(),
                details: Some(
                    "This deployment token is scoped to a different project and cannot access this resource"
                        .to_string(),
                ),
            }),
        ));
    }
    require_project_access(auth, project_id, project_access_checker)
        .await
        .map_err(problem_to_error_response)?;
    Ok(())
}

/// Confines a human session to the projects/teams they belong to. No-op when
/// no `ProjectAccessChecker` is registered (plain OSS); enforced when EE
/// Teams installs one. Isolated in its own `async fn` returning `Problem`
/// because `project_access_guard!` does a bare `return Err(problem)` and so
/// requires that exact return type.
async fn require_project_access(
    auth: &AuthContext,
    project_id: i32,
    project_access_checker: &Option<Arc<dyn temps_core::ProjectAccessChecker>>,
) -> Result<(), Problem> {
    project_access_guard!(auth, project_id, project_access_checker);
    Ok(())
}

/// Converts a `Problem` (RFC 7807) produced by `project_access_guard!` into
/// this crate's pre-existing `(StatusCode, Json<ErrorResponse>)` error shape.
fn problem_to_error_response(problem: Problem) -> (StatusCode, Json<ErrorResponse>) {
    let status = problem.status_code;
    let error = problem
        .body
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Forbidden")
        .to_string();
    let details = problem
        .body
        .get("detail")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    (status, Json(ErrorResponse { error, details }))
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct PerformanceMetricsQuery {
    start_date: DateTime,
    end_date: DateTime,
    project_id: i32,
    environment_id: Option<i32>,
    deployment_id: Option<i32>,
    /// Device type filter: "desktop" or "mobile"
    device_type: Option<String>,
    /// Include crawler/datacenter (bot) samples. Defaults to false — bots
    /// are excluded from the read view but always stored at ingest.
    include_bots: Option<bool>,
    /// Segment filters (filter_path, filter_country, filter_region,
    /// filter_city, filter_browser, filter_operating_system) — flattened so
    /// each remains a top-level query string param.
    #[serde(flatten)]
    segment: SpeedSegmentFilters,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct GroupedPageMetricsQuery {
    start_date: DateTime,
    end_date: DateTime,
    project_id: i32,
    environment_id: Option<i32>,
    deployment_id: Option<i32>,
    /// Device type filter: "desktop" or "mobile"
    device_type: Option<String>,
    /// Include crawler/datacenter (bot) samples. Defaults to false.
    include_bots: Option<bool>,
    // "path", "country", "region", "city", "device_type", "browser", "operating_system"
    group_by: String,
    /// Segment filters — same shape as `PerformanceMetricsQuery`.
    #[serde(flatten)]
    segment: SpeedSegmentFilters,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct HasMetricsQuery {
    project_id: i32,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HasMetricsResponse {
    pub has_metrics: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
    pub details: Option<String>,
}

/// Speed metrics payload for recording web vitals
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpeedMetricsPayload {
    /// Time to First Byte (milliseconds)
    pub ttfb: Option<f32>,
    /// Largest Contentful Paint (milliseconds)
    pub lcp: Option<f32>,
    /// First Input Delay (milliseconds)
    pub fid: Option<f32>,
    /// First Contentful Paint (milliseconds)
    pub fcp: Option<f32>,
    /// Cumulative Layout Shift (score)
    pub cls: Option<f32>,
    /// Interaction to Next Paint (milliseconds)
    pub inp: Option<f32>,
    /// Screen width in pixels
    pub screen_width: Option<i16>,
    /// Screen height in pixels
    pub screen_height: Option<i16>,
    /// Viewport width in pixels
    pub viewport_width: Option<i16>,
    /// Viewport height in pixels
    pub viewport_height: Option<i16>,
    /// Browser language
    pub language: Option<String>,
    /// Page pathname
    pub pathname: Option<String>,
    /// Query string
    pub query: Option<String>,
    /// Client-generated visitor id, used only when the request carries no
    /// Temps-issued `_temps_visitor_id` cookie — i.e. Temps is used purely as
    /// an analytics backend for an app it doesn't deploy/proxy (gotempsh/temps#848).
    pub visitor_id: Option<String>,
    /// Client-generated session id fallback (see `visitor_id`).
    pub session_id: Option<String>,
}

/// Update speed metrics payload for late-loading metrics
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSpeedMetricsPayload {
    /// Cumulative Layout Shift (score)
    pub cls: Option<f32>,
    /// Interaction to Next Paint (milliseconds)
    pub inp: Option<f32>,
    /// Client-generated visitor id fallback (see [`SpeedMetricsPayload::visitor_id`]).
    /// Required to identify the right row on the keyed path, where there is
    /// no Temps-issued cookie to fall back on.
    pub visitor_id: Option<String>,
    /// Client-generated session id fallback (see [`SpeedMetricsPayload::visitor_id`]).
    pub session_id: Option<String>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_performance_metrics,
        get_metrics_over_time,
        get_grouped_page_metrics,
        has_performance_metrics,
        record_speed_metrics,
        update_speed_metrics
    ),
    components(
        schemas(
            PerformanceMetricsResponse,
            MetricsOverTimeResponse,
            GroupedPageMetricsResponse,
            GroupedPageMetric,
            PerformanceMetricsQuery,
            GroupedPageMetricsQuery,
            HasMetricsQuery,
            HasMetricsResponse,
            SpeedMetricsPayload,
            UpdateSpeedMetricsPayload,
            ErrorResponse
        )
    ),
    tags(
        (name = "Performance", description = "Performance metrics management")
    )
)]
pub struct PerformanceApiDoc;

/// Admin routes for performance metrics (dashboard queries).
pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/performance/metrics", get(get_performance_metrics))
        .route("/performance/metrics-over-time", get(get_metrics_over_time))
        .route("/performance/page-metrics", get(get_grouped_page_metrics))
        .route("/performance/has-metrics", get(has_performance_metrics))
}

/// Public ingest routes for performance metrics — called by browser SDKs.
pub fn configure_public_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/_temps/speed", post(record_speed_metrics))
        .route("/_temps/speed/update", post(update_speed_metrics))
        .layer(public_ingest_cors())
}

/// CORS for the public performance ingest routes.
///
/// Required by ADR-040: with an ingest key the request is cross-origin by
/// definition, and without this layer the browser blocks it before it ever
/// leaves the page.
///
/// `allow_credentials` stays at its default `false`, and must never be set
/// true. Key-based ingest needs no cookies by design; credentialed CORS on a
/// wildcard origin would be a real vulnerability, and browsers reject the
/// combination outright.
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

/// Get performance metrics
#[utoipa::path(
    tag = "Performance",
    get,
    path = "/performance/metrics",
    params(
        ("start_date" = String, Query, description = "Start date in format YYYY-MM-DD HH:MM:SS"),
        ("end_date" = String, Query, description = "End date in format YYYY-MM-DD HH:MM:SS"),
        ("project_id" = i32, Query, description = "Project ID or slug"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("deployment_id" = Option<i32>, Query, description = "Deployment ID (optional)"),
        ("device_type" = Option<String>, Query, description = "Device type filter: desktop or mobile (optional)"),
        ("include_bots" = Option<bool>, Query, description = "Include crawler/datacenter bot samples (default false)"),
        ("filter_path" = Option<String>, Query, description = "Filter to one page pathname (optional)"),
        ("filter_country" = Option<String>, Query, description = "Filter to one country (optional)"),
        ("filter_region" = Option<String>, Query, description = "Filter to one region (optional)"),
        ("filter_city" = Option<String>, Query, description = "Filter to one city (optional)"),
        ("filter_browser" = Option<String>, Query, description = "Filter to one browser (optional)"),
        ("filter_operating_system" = Option<String>, Query, description = "Filter to one operating system (optional)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved performance metrics", body = PerformanceMetricsResponse),
        (status = 400, description = "Invalid date format or missing parameters", body = ErrorResponse),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Insufficient permissions", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn get_performance_metrics(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<PerformanceMetricsQuery>,
) -> Result<Json<PerformanceMetricsResponse>, (StatusCode, Json<ErrorResponse>)> {
    require_analytics_read(&auth, query.project_id, &state.project_access_checker).await?;

    match state
        .performance_service
        .get_metrics(
            query.start_date.into(),
            query.end_date.into(),
            query.project_id,
            query.environment_id,
            query.deployment_id,
            query.device_type,
            &query.segment,
            query.include_bots.unwrap_or(false),
        )
        .await
    {
        Ok(metrics) => Ok(Json(metrics)),
        Err(e) => {
            error!("Error fetching performance metrics: {:?}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to fetch performance metrics".to_string(),
                    details: Some(format!("Error retrieving metrics: {:?}", e)),
                }),
            ))
        }
    }
}

/// Get metrics over time
#[utoipa::path(
    tag = "Performance",
    get,
    path = "/performance/metrics-over-time",
    params(
        ("start_date" = String, Query, description = "Start date in format YYYY-MM-DDTHH:MM:SSZ"),
        ("end_date" = String, Query, description = "End date in format YYYY-MM-DDTHH:MM:SSZ"),
        ("project_id" = i32, Query, description = "Project ID or slug"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("deployment_id" = Option<i32>, Query, description = "Deployment ID (optional)"),
        ("device_type" = Option<String>, Query, description = "Device type filter: desktop or mobile (optional)"),
        ("include_bots" = Option<bool>, Query, description = "Include crawler/datacenter bot samples (default false)"),
        ("filter_path" = Option<String>, Query, description = "Filter to one page pathname (optional)"),
        ("filter_country" = Option<String>, Query, description = "Filter to one country (optional)"),
        ("filter_region" = Option<String>, Query, description = "Filter to one region (optional)"),
        ("filter_city" = Option<String>, Query, description = "Filter to one city (optional)"),
        ("filter_browser" = Option<String>, Query, description = "Filter to one browser (optional)"),
        ("filter_operating_system" = Option<String>, Query, description = "Filter to one operating system (optional)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved metrics over time", body = MetricsOverTimeResponse),
        (status = 400, description = "Invalid date format or missing parameters", body = ErrorResponse),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Insufficient permissions", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn get_metrics_over_time(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<PerformanceMetricsQuery>,
) -> Result<Json<MetricsOverTimeResponse>, (StatusCode, Json<ErrorResponse>)> {
    require_analytics_read(&auth, query.project_id, &state.project_access_checker).await?;

    match state
        .performance_service
        .get_metrics_over_time(
            query.start_date.into(),
            query.end_date.into(),
            query.project_id,
            query.environment_id,
            query.deployment_id,
            query.device_type,
            &query.segment,
            query.include_bots.unwrap_or(false),
        )
        .await
    {
        Ok(metrics) => Ok(Json(metrics)),
        Err(e) => {
            error!("Error fetching metrics over time: {:?}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to fetch metrics over time".to_string(),
                    details: Some(format!("Error retrieving metrics: {:?}", e)),
                }),
            ))
        }
    }
}

/// Get grouped page metrics
#[utoipa::path(
    tag = "Performance",
    get,
    path = "/performance/page-metrics",
    params(
        ("start_date" = String, Query, description = "Start date in format YYYY-MM-DDTHH:MM:SSZ"),
        ("end_date" = String, Query, description = "End date in format YYYY-MM-DDTHH:MM:SSZ"),
        ("project_id" = i32, Query, description = "Project ID or slug"),
        ("environment_id" = Option<i32>, Query, description = "Environment ID (optional)"),
        ("deployment_id" = Option<i32>, Query, description = "Deployment ID (optional)"),
        ("group_by" = String, Query, description = "Group by: path, country, region, city, device_type, browser, operating_system"),
        ("device_type" = Option<String>, Query, description = "Device type filter: desktop or mobile (optional)"),
        ("include_bots" = Option<bool>, Query, description = "Include crawler/datacenter bot samples (default false)"),
        ("filter_path" = Option<String>, Query, description = "Filter to one page pathname (optional)"),
        ("filter_country" = Option<String>, Query, description = "Filter to one country (optional)"),
        ("filter_region" = Option<String>, Query, description = "Filter to one region (optional)"),
        ("filter_city" = Option<String>, Query, description = "Filter to one city (optional)"),
        ("filter_browser" = Option<String>, Query, description = "Filter to one browser (optional)"),
        ("filter_operating_system" = Option<String>, Query, description = "Filter to one operating system (optional)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved grouped page metrics", body = GroupedPageMetricsResponse),
        (status = 400, description = "Invalid date format, missing parameters, or invalid group_by value", body = ErrorResponse),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Insufficient permissions", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn get_grouped_page_metrics(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<GroupedPageMetricsQuery>,
) -> Result<Json<GroupedPageMetricsResponse>, (StatusCode, Json<ErrorResponse>)> {
    require_analytics_read(&auth, query.project_id, &state.project_access_checker).await?;

    let group_by = match query.group_by.as_str() {
        "path" => GroupBy::Path,
        "country" => GroupBy::Country,
        "region" => GroupBy::Region,
        "city" => GroupBy::City,
        "device_type" => GroupBy::DeviceType,
        "browser" => GroupBy::Browser,
        "operating_system" => GroupBy::OperatingSystem,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Invalid group_by parameter".to_string(),
                    details: Some(
                        "group_by must be one of: path, country, region, city, device_type, browser, operating_system"
                            .to_string(),
                    ),
                }),
            ))
        }
    };

    match state
        .performance_service
        .get_grouped_page_metrics(
            query.start_date.into(),
            query.end_date.into(),
            query.project_id,
            query.environment_id,
            query.deployment_id,
            query.device_type.clone(),
            &query.segment,
            query.include_bots.unwrap_or(false),
            group_by,
        )
        .await
    {
        Ok(metrics) => Ok(Json(metrics)),
        Err(e) => {
            error!("Error fetching grouped page metrics: {:?}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to fetch grouped page metrics".to_string(),
                    details: Some(format!("Error retrieving metrics: {:?}", e)),
                }),
            ))
        }
    }
}

/// Check if performance metrics exist for a project
#[utoipa::path(
    tag = "Performance",
    get,
    path = "/performance/has-metrics",
    params(
        ("project_id" = i32, Query, description = "Project ID")
    ),
    responses(
        (status = 200, description = "Successfully checked performance metrics availability", body = HasMetricsResponse),
        (status = 401, description = "Authentication required", body = ErrorResponse),
        (status = 403, description = "Insufficient permissions", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
async fn has_performance_metrics(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(query): Query<HasMetricsQuery>,
) -> Result<Json<HasMetricsResponse>, (StatusCode, Json<ErrorResponse>)> {
    require_analytics_read(&auth, query.project_id, &state.project_access_checker).await?;

    match state
        .performance_service
        .has_metrics(query.project_id)
        .await
    {
        Ok(has_metrics) => Ok(Json(HasMetricsResponse { has_metrics })),
        Err(e) => {
            error!("Error checking performance metrics: {:?}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to check performance metrics".to_string(),
                    details: Some(format!("Error checking metrics: {:?}", e)),
                }),
            ))
        }
    }
}

/// The scope a performance metric is attributed to.
#[derive(Debug, PartialEq, Eq)]
struct MetricScope {
    project_id: i32,
    /// `None` when the route has no environment — an app Temps does not deploy.
    environment_id: Option<i32>,
    /// `None` when the environment has no live Temps deployment.
    deployment_id: Option<i32>,
}

/// Derive the metric scope from a resolved route.
///
/// **Only a missing project drops the event.** A route without an environment
/// or without a deployment is a normal, recordable state: the environment may
/// simply have nothing deployed right now, and (per ADR-040) the app may not be
/// Temps-deployed at all. Both handlers previously early-returned `204` in
/// those cases, which looked like success to the browser SDK while silently
/// discarding every web vital. Both DB columns are nullable, so pass the
/// `Option` through instead of dropping the row.
fn metric_scope_from_route(route: &temps_routes::RouteInfo) -> Option<MetricScope> {
    // No project means a sandbox/orphaned route with nothing to attribute the
    // metric to; `project_id` is `NOT NULL` with a real FK, so there is no
    // value we could write.
    let project = route.project.as_ref()?;
    Some(MetricScope {
        project_id: project.id,
        environment_id: route.environment.as_ref().map(|e| e.id),
        deployment_id: route.deployment.as_ref().map(|d| d.id),
    })
}

impl From<temps_analytics::ResolvedIngestScope> for MetricScope {
    /// A resolved ingest key **replaces** Host-based resolution (ADR-040 §3) —
    /// it is never merged with a route lookup.
    fn from(scope: temps_analytics::ResolvedIngestScope) -> Self {
        Self {
            project_id: scope.project_id,
            environment_id: scope.environment_id,
            deployment_id: scope.deployment_id,
        }
    }
}

/// Record performance metrics from client
#[utoipa::path(
    tag = "Performance",
    post,
    path = "/_temps/speed",
    request_body = SpeedMetricsPayload,
    params(
        ("x-temps-analytics-key" = Option<String>, Header, description = "Analytics ingest key (ADR-040), `pa_` followed by 64 hex characters. An alternative to Host-based project resolution, for apps Temps does not deploy and which therefore have no route-table entry. When present it takes precedence and the Host header is not consulted for resolution; a key that does not resolve to an active row is a 401, never a fallback to Host. The value is public by design — it ships in client JS — and is write-only: it grants analytics ingest for one project (optionally one environment) and nothing else."),
        ("temps_key" = Option<String>, Query, description = "Query-string fallback for the analytics ingest key, for clients that cannot set custom headers (`navigator.sendBeacon`, used for page-unload events). Consulted only when the `x-temps-analytics-key` header is absent; identical precedence and error semantics.")
    ),
    responses(
        (status = 204, description = "Metrics recorded successfully"),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 404, description = "Host not found in route table", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn record_speed_metrics(
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
    Json(payload): Json<SpeedMetricsPayload>,
) -> impl IntoResponse {
    info!("Recording speed metrics from client");

    // Host comes from `RequestMetadata`, which the auth middleware already
    // normalizes by stripping any ":port" suffix. That matches the proxy's
    // route-table keying so `get_route` works correctly even on non-default
    // ports (e.g. the :8080 dev proxy).
    let host = metadata.host.clone();

    // ADR-040 §3. A presented ingest key resolves the scope outright; `Host` is
    // never consulted for resolution in that branch, and an unresolvable key is
    // a 401 rather than a silent fall-through to `Host`.
    let (scope, is_keyed) = if let Some(key) = extract_analytics_key(&headers, raw_query.as_deref())
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
            Ok(resolved) => {
                info!(
                    "Resolved analytics ingest key {} to project={}, env={:?}, deploy={:?}",
                    resolved.key_id,
                    resolved.project_id,
                    resolved.environment_id,
                    resolved.deployment_id
                );
                (MetricScope::from(resolved), true)
            }
            Err(problem) => return problem.into_response(),
        }
    } else {
        if host.is_empty() {
            error!("Missing Host header");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Missing Host header"
                })),
            )
                .into_response();
        }

        // Look up project/environment/deployment from route table
        match state.route_table.get_route(&host) {
            Some(route_info) => {
                let Some(scope) = metric_scope_from_route(&route_info) else {
                    info!(
                        "Dropping performance event for host {} — no associated project",
                        host
                    );
                    return StatusCode::NO_CONTENT.into_response();
                };

                info!(
                    "Resolved host {} to project={}, env={:?}, deploy={:?}",
                    host, scope.project_id, scope.environment_id, scope.deployment_id
                );

                (scope, false)
            }
            None => {
                error!("Host {} not found in route table", host);
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": format!("Host {} not found", host)
                    })),
                )
                    .into_response();
            }
        }
    };

    // Lookup IP geolocation
    let ip_address_id = if !metadata.ip_address.is_empty() {
        match state
            .ip_address_service
            .get_or_create_ip(&metadata.ip_address)
            .await
        {
            Ok(ip_info) => Some(ip_info.id),
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

    // Extract User-Agent header
    let user_agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

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

    match state
        .performance_service
        .record_performance_metrics(RecordPerformanceMetricsConfig {
            project_id: scope.project_id,
            environment_id: scope.environment_id,
            deployment_id: scope.deployment_id,
            session_id,
            visitor_id,
            ip_address_id,
            ttfb: payload.ttfb,
            lcp: payload.lcp,
            fid: payload.fid,
            fcp: payload.fcp,
            cls: payload.cls,
            inp: payload.inp,
            pathname: payload.pathname,
            query: payload.query,
            // Still read and stored on the keyed path (ADR-040 §3), where it is
            // data rather than a lookup key — and where it can legitimately be
            // absent, since Temps never served this page.
            host: (!host.is_empty()).then_some(host),
            user_agent,
            screen_width: payload.screen_width,
            screen_height: payload.screen_height,
            viewport_width: payload.viewport_width,
            viewport_height: payload.viewport_height,
            language: payload.language,
        })
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            error!("Failed to record speed metrics: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to record speed metrics",
                    "details": format!("{:?}", e)
                })),
            )
                .into_response()
        }
    }
}

/// Update late performance metrics
#[utoipa::path(
    tag = "Performance",
    post,
    path = "/_temps/speed/update",
    request_body = UpdateSpeedMetricsPayload,
    params(
        ("x-temps-analytics-key" = Option<String>, Header, description = "Analytics ingest key (ADR-040), `pa_` followed by 64 hex characters. An alternative to Host-based project resolution, for apps Temps does not deploy and which therefore have no route-table entry. When present it takes precedence and the Host header is not consulted for resolution; a key that does not resolve to an active row is a 401, never a fallback to Host. The value is public by design — it ships in client JS — and is write-only: it grants analytics ingest for one project (optionally one environment) and nothing else."),
        ("temps_key" = Option<String>, Query, description = "Query-string fallback for the analytics ingest key, for clients that cannot set custom headers. This endpoint is called via `navigator.sendBeacon` on page unload, so the query form is the only one available there. Consulted only when the `x-temps-analytics-key` header is absent; identical precedence and error semantics.")
    ),
    responses(
        (status = 204, description = "Metrics updated successfully"),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 404, description = "Host not found or metrics not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn update_speed_metrics(
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
    Json(payload): Json<UpdateSpeedMetricsPayload>,
) -> impl IntoResponse {
    info!("Updating late performance metrics from client");

    // Host comes from `RequestMetadata`, which the auth middleware already
    // normalizes by stripping any ":port" suffix. That matches the proxy's
    // route-table keying so `get_route` works correctly even on non-default
    // ports (e.g. the :8080 dev proxy).
    let host = metadata.host.clone();

    // ADR-040 §3. This route is called via `navigator.sendBeacon`, which cannot
    // set headers — so the `?temps_key=` fallback is the only way a keyed
    // client can authenticate here, and `extract_analytics_key` accepts both.
    let (scope, is_keyed) = if let Some(key) = extract_analytics_key(&headers, raw_query.as_deref())
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
            Ok(resolved) => {
                info!(
                    "Resolved analytics ingest key {} to project={}, env={:?}, deploy={:?}",
                    resolved.key_id,
                    resolved.project_id,
                    resolved.environment_id,
                    resolved.deployment_id
                );
                (MetricScope::from(resolved), true)
            }
            Err(problem) => return problem.into_response(),
        }
    } else {
        if host.is_empty() {
            error!("Missing Host header");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Missing Host header"
                })),
            )
                .into_response();
        }

        // Look up project/environment/deployment from route table. The update must
        // resolve to the same scope the original insert used — including when that
        // scope was NULL — so it goes through the same helper.
        match state.route_table.get_route(&host) {
            Some(route_info) => {
                let Some(scope) = metric_scope_from_route(&route_info) else {
                    info!(
                        "Dropping performance update for host {} — no associated project",
                        host
                    );
                    return StatusCode::NO_CONTENT.into_response();
                };

                info!(
                    "Resolved host {} to project={}, env={:?}, deploy={:?}",
                    host, scope.project_id, scope.environment_id, scope.deployment_id
                );

                (scope, false)
            }
            None => {
                error!("Host {} not found in route table", host);
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": format!("Host {} not found", host)
                    })),
                )
                    .into_response();
            }
        }
    };

    // On the keyed path there is never a Temps cookie (ADR-040 §3), so
    // identity has to come from the payload — resolving it the same way
    // `record_speed_metrics` does. Without this, both filters below would be
    // `None` and the update would silently land on whichever row is newest
    // for the project/env/deployment scope, i.e. a different visitor's data.
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

    if is_keyed && session_id.is_none() && visitor_id.is_none() {
        error!(
            "Keyed speed-metrics update carries no resolvable identity; refusing to guess a row"
        );
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "No visitorId or sessionId in the request body — required on the keyed ingest path"
            })),
        )
            .into_response();
    }

    match state
        .performance_service
        .update_performance_metrics(UpdatePerformanceMetricsConfig {
            project_id: scope.project_id,
            environment_id: scope.environment_id,
            deployment_id: scope.deployment_id,
            session_id,
            visitor_id,
            cls: payload.cls,
            inp: payload.inp,
        })
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            error!("Failed to update speed metrics: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to update speed metrics",
                    "details": format!("{:?}", e)
                })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use temps_auth::Role;
    use temps_core::ProjectAccessChecker;
    use temps_entities::deployments::Model as DeploymentModel;

    fn test_project(id: i32) -> temps_entities::projects::Model {
        let now = chrono::Utc::now();
        temps_entities::projects::Model {
            id,
            name: "test-project".to_string(),
            repo_name: "test-repo".to_string(),
            repo_owner: "test-owner".to_string(),
            directory: String::new(),
            main_branch: "main".to_string(),
            preset: temps_entities::preset::Preset::NextJs,
            preset_config: None,
            deployment_config: None,
            created_at: now,
            updated_at: now,
            slug: "test-project".to_string(),
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: false,
            git_url: None,
            git_provider_connection_id: None,
            attack_mode: false,
            ai_alert_summaries_enabled: None,
            ai_debug_chat_enabled: None,
            ai_write_actions_enabled: false,
            error_source_context_enabled: false,
            error_source_root: None,
            enable_preview_environments: false,
            preview_envs_on_demand: false,
            preview_envs_idle_timeout_seconds: 300,
            preview_envs_wake_timeout_seconds: 30,
            source_type: temps_entities::source_type::SourceType::Git,
            allow_alternate_sources: None,
            template_slug: None,
            gitlab_webhook_id: None,
            gitlab_webhook_signing_token: None,
            gitea_webhook_signing_token: None,
            bitbucket_webhook_token: None,
            bitbucket_webhook_hook_id: None,
            generic_webhook_token: None,
            cross_project_trace_sharing: true,
            ai_api_traffic_summary_enabled: None,
            image_retention_hours: None,
        }
    }

    fn test_environment(id: i32, project_id: i32) -> temps_entities::environments::Model {
        let now = chrono::Utc::now();
        temps_entities::environments::Model {
            id,
            name: "production".to_string(),
            slug: "production".to_string(),
            subdomain: "prod".to_string(),
            last_deployment: None,
            host: "app.example.com".to_string(),
            upstreams: Default::default(),
            created_at: now,
            updated_at: now,
            project_id,
            current_deployment_id: None,
            branch: None,
            deleted_at: None,
            deployment_config: None,
            is_preview: false,
            protected: false,
            sleeping: false,
            attack_mode: None,
            force_https: None,
            last_activity_at: None,
        }
    }

    fn test_deployment(id: i32, project_id: i32, environment_id: i32) -> DeploymentModel {
        let now = chrono::Utc::now();
        DeploymentModel {
            id,
            project_id,
            environment_id,
            created_at: now,
            updated_at: now,
            slug: "deploy-1".to_string(),
            state: "ready".to_string(),
            metadata: None,
            deploying_at: None,
            ready_at: None,
            started_at: None,
            finished_at: None,
            context_vars: None,
            branch_ref: None,
            tag_ref: None,
            commit_sha: None,
            commit_message: None,
            commit_author: None,
            commit_json: None,
            cancelled_reason: None,
            static_dir_location: None,
            screenshot_location: None,
            image_name: None,
            deployment_config: None,
            promoted_from_deployment_id: None,
        }
    }

    fn test_route(
        project: Option<temps_entities::projects::Model>,
        environment: Option<temps_entities::environments::Model>,
        deployment: Option<DeploymentModel>,
    ) -> temps_routes::RouteInfo {
        temps_routes::RouteInfo {
            backend: temps_routes::BackendType::StaticDir {
                path: "/tmp".to_string(),
            },
            redirect_to: None,
            status_code: None,
            project: project.map(Arc::new),
            environment: environment.map(Arc::new),
            deployment: deployment.map(Arc::new),
            cert_eligible: false,
        }
    }

    /// Regression test for the silent web-vitals loss fixed alongside ADR-040:
    /// a project whose route has no deployment (nothing deployed right now, or
    /// an app Temps does not deploy at all) used to early-return `204` from
    /// both `/speed` and `/speed/update`, which looks like success to the SDK
    /// while discarding every metric. The scope must resolve with `None`s, not
    /// vanish.
    #[test]
    fn metric_scope_records_project_without_environment_or_deployment() {
        let route = test_route(Some(test_project(7)), None, None);

        let scope = metric_scope_from_route(&route)
            .expect("a project without environment/deployment must still be recorded");

        assert_eq!(
            scope,
            MetricScope {
                project_id: 7,
                environment_id: None,
                deployment_id: None,
            }
        );
    }

    /// An environment that exists but currently has nothing deployed is the
    /// other half of the same bug — `deployment_id` alone must be allowed to
    /// be `None` without losing the environment attribution.
    #[test]
    fn metric_scope_records_environment_without_deployment() {
        let route = test_route(Some(test_project(7)), Some(test_environment(3, 7)), None);

        let scope = metric_scope_from_route(&route)
            .expect("an environment without a deployment must still be recorded");

        assert_eq!(
            scope,
            MetricScope {
                project_id: 7,
                environment_id: Some(3),
                deployment_id: None,
            }
        );
    }

    #[test]
    fn metric_scope_carries_full_attribution_when_deployed() {
        let route = test_route(
            Some(test_project(7)),
            Some(test_environment(3, 7)),
            Some(test_deployment(11, 7, 3)),
        );

        let scope = metric_scope_from_route(&route).expect("fully resolved route must be recorded");

        assert_eq!(
            scope,
            MetricScope {
                project_id: 7,
                environment_id: Some(3),
                deployment_id: Some(11),
            }
        );
    }

    /// The one case that *is* still a drop: `performance_metrics.project_id` is
    /// `NOT NULL` with a real FK, so a sandbox/orphaned route has no value that
    /// could be written.
    #[test]
    fn metric_scope_drops_route_without_project() {
        let route = test_route(None, None, None);

        assert!(metric_scope_from_route(&route).is_none());
    }

    fn test_user_auth(role: Role) -> AuthContext {
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
        AuthContext::new_session(user, role)
    }

    /// A mock [`ProjectAccessChecker`] that returns a fixed outcome, mirroring
    /// `temps_auth::permission_guard::tests::MockChecker`.
    struct MockChecker {
        allow: bool,
    }

    #[async_trait]
    impl ProjectAccessChecker for MockChecker {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self.allow)
        }
    }

    /// Regression test for the cross-tenant IDOR this crate previously had:
    /// a plain `Role::User` holds `AnalyticsRead` instance-wide, so before
    /// wiring `project_access_guard!` in, this call would have returned `Ok`
    /// for ANY `project_id`, letting a user read another team's performance
    /// data just by passing its `project_id`.
    #[tokio::test]
    async fn require_analytics_read_denies_user_without_project_access() {
        let auth = test_user_auth(Role::User);
        let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
            Some(Arc::new(MockChecker { allow: false }));

        let result = require_analytics_read(&auth, 42, &checker).await;

        let (status, body) = result.expect_err("user without project access must be denied");
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body.0.error, "Project Access Denied");
    }

    #[tokio::test]
    async fn require_analytics_read_allows_user_with_project_access() {
        let auth = test_user_auth(Role::User);
        let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
            Some(Arc::new(MockChecker { allow: true }));

        let result = require_analytics_read(&auth, 42, &checker).await;

        assert!(result.is_ok());
    }

    /// Plain OSS: no `ProjectAccessChecker` registered — must reduce to the
    /// instance-wide permission check only (no-op for the project narrowing).
    #[tokio::test]
    async fn require_analytics_read_no_checker_registered_is_no_op() {
        let auth = test_user_auth(Role::User);

        let result = require_analytics_read(&auth, 42, &None).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn require_analytics_read_admin_bypasses_project_narrowing() {
        let auth = test_user_auth(Role::Admin);
        // Even a deny-everything checker must not block an instance admin.
        let checker: Option<Arc<dyn temps_core::ProjectAccessChecker>> =
            Some(Arc::new(MockChecker { allow: false }));

        let result = require_analytics_read(&auth, 42, &checker).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn require_analytics_read_denies_missing_permission() {
        // A role with no AnalyticsRead permission at all must be denied
        // outright, before the project-access check ever runs.
        let user = temps_entities::users::Model {
            id: 1,
            name: "Restricted User".to_string(),
            email: "restricted@example.com".to_string(),
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
        let auth = AuthContext::new_session(user, Role::MetricsIngest);

        let result = require_analytics_read(&auth, 42, &None).await;

        let (status, body) = result.expect_err("role without AnalyticsRead must be denied");
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body.0.error, "Insufficient permissions");
    }

    // ── ADR-040: keyed ingest on POST /_temps/speed and /_temps/speed/update ─
    //
    // The no-key regression cases matter most: breaking web-vitals ingest for a
    // Temps-deployed app would be worse than not shipping keyed ingest at all.

    use axum::body::Body;
    use axum::http::Request;
    use axum::middleware;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
    use temps_analytics::ingest_keys::{AnalyticsIngestKeyService, AnalyticsIngestRateLimiter};
    use temps_database::test_utils::TestDatabase;
    use tower::ServiceExt;

    /// A syntactically valid key that is guaranteed not to exist.
    const UNKNOWN_KEY: &str = "pa_0000000000000000000000000000000000000000000000000000000000000000";

    async fn insert_db_project(
        db: &sea_orm::DatabaseConnection,
    ) -> temps_entities::projects::Model {
        temps_entities::projects::ActiveModel {
            name: Set("test-project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            slug: Set("test-project".to_string()),
            source_type: Set(temps_entities::source_type::SourceType::Git),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("Failed to insert test project")
    }

    fn build_state(db: Arc<sea_orm::DatabaseConnection>) -> Arc<AppState> {
        let geoip_service = Arc::new(temps_geo::GeoIpService::Mock(
            temps_geo::MockGeoIpService::new(),
        ));
        Arc::new(AppState {
            performance_service: Arc::new(crate::services::service::PerformanceService::new(
                db.clone(),
            )),
            route_table: Arc::new(temps_routes::CachedPeerTable::new(db.clone())),
            ip_address_service: Arc::new(temps_geo::IpAddressService::new(
                db.clone(),
                geoip_service,
            )),
            project_access_checker: None,
            ingest_key_service: Arc::new(AnalyticsIngestKeyService::new(db.clone())),
            ingest_rate_limiter: Arc::new(AnalyticsIngestRateLimiter::new()),
        })
    }

    fn test_request_metadata(headers: &HeaderMap) -> temps_core::RequestMetadata {
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

    fn insert_test_route(
        route_table: &temps_routes::CachedPeerTable,
        host: &str,
        project: Option<temps_entities::projects::Model>,
    ) {
        route_table.insert_route_for_test(host, test_route(project, None, None));
    }

    fn speed_payload() -> serde_json::Value {
        serde_json::json!({ "ttfb": 120.0, "lcp": 900.0, "pathname": "/pricing" })
    }

    async fn stored_metrics(
        db: &sea_orm::DatabaseConnection,
    ) -> Vec<temps_entities::performance_metrics::Model> {
        temps_entities::performance_metrics::Entity::find()
            .all(db)
            .await
            .expect("Failed to query performance metrics")
    }

    #[tokio::test]
    async fn keyed_speed_ingest_resolves_the_keys_project_without_the_route_table() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_db_project(db.as_ref()).await;
        let state = build_state(db.clone());

        let key = state
            .ingest_key_service
            .create(project.id, None, None, None, None, None)
            .await
            .expect("minting an ingest key must succeed");

        let response = setup_public_app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/speed")
                    .header("host", "app.not-deployed-by-temps.test")
                    .header("content-type", "application/json")
                    .header("x-temps-analytics-key", &key.public_key)
                    .body(Body::from(speed_payload().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let metrics = stored_metrics(db.as_ref()).await;
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].project_id, project.id);
        assert_eq!(metrics[0].environment_id, None);
        assert_eq!(metrics[0].deployment_id, None);
        assert_eq!(
            metrics[0].host.as_deref(),
            Some("app.not-deployed-by-temps.test")
        );

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn keyed_speed_update_accepts_the_query_param_fallback() {
        // `/speed/update` is delivered by `navigator.sendBeacon`, which cannot
        // set headers — the query param is the only usable transport there.
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_db_project(db.as_ref()).await;
        let state = build_state(db.clone());

        let key = state
            .ingest_key_service
            .create(project.id, None, None, None, None, None)
            .await
            .expect("minting an ingest key must succeed");

        // The keyed path never carries a Temps cookie, so `visitorId` in the
        // payload is the only way to bind the update to the row the insert
        // created — see `resolve_client_identity`.
        let mut seed_payload = speed_payload();
        seed_payload["visitorId"] = serde_json::json!("keyed-update-test-visitor");

        // Seed the row the late-metrics call will update.
        let response = setup_public_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/speed")
                    .header("host", "app.not-deployed-by-temps.test")
                    .header("content-type", "application/json")
                    .header("x-temps-analytics-key", &key.public_key)
                    .body(Body::from(seed_payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = setup_public_app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/_temps/speed/update?temps_key={}", key.public_key))
                    .header("host", "app.not-deployed-by-temps.test")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "cls": 0.02,
                            "inp": 40.0,
                            "visitorId": "keyed-update-test-visitor",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // The update must land on the row the insert created, matched by the
        // shared client-generated visitorId — not by guessing "most recent row".
        let metrics = stored_metrics(db.as_ref()).await;
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].cls, Some(0.02));
        assert_eq!(metrics[0].inp, Some(40.0));

        test_db.cleanup().await;
    }

    /// Mirrors the fix for the cross-visitor-corruption bug this replaces:
    /// a keyed update with no resolvable identity must be rejected outright
    /// rather than silently landing on "whichever row is newest."
    #[tokio::test]
    async fn keyed_speed_update_without_identity_is_rejected() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_db_project(db.as_ref()).await;
        let state = build_state(db.clone());

        let key = state
            .ingest_key_service
            .create(project.id, None, None, None, None, None)
            .await
            .expect("minting an ingest key must succeed");

        let response = setup_public_app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/_temps/speed/update?temps_key={}", key.public_key))
                    .header("host", "app.not-deployed-by-temps.test")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "cls": 0.02, "inp": 40.0 }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(stored_metrics(db.as_ref()).await.len(), 0);

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn invalid_speed_key_returns_401_and_never_falls_back_to_host() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_db_project(db.as_ref()).await;
        let state = build_state(db.clone());
        // The Host *would* resolve — a typo'd key must not silently use it.
        insert_test_route(&state.route_table, "app.example.test", Some(project));

        for uri in ["/_temps/speed", "/_temps/speed/update"] {
            let response = setup_public_app(state.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header("host", "app.example.test")
                        .header("content-type", "application/json")
                        .header("x-temps-analytics-key", UNKNOWN_KEY)
                        .body(Body::from(speed_payload().to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }

        assert!(
            stored_metrics(db.as_ref()).await.is_empty(),
            "a rejected key must not fall through to Host-based resolution"
        );

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn scoped_speed_key_enforces_the_origin_allowlist() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_db_project(db.as_ref()).await;
        let state = build_state(db.clone());

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
                    .uri("/_temps/speed")
                    .header("host", "app.example.com")
                    .header("content-type", "application/json")
                    .header("x-temps-analytics-key", public_key);
                if let Some(origin) = origin {
                    builder = builder.header("origin", origin);
                }
                app.oneshot(
                    builder
                        .body(Body::from(speed_payload().to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
            }
        };

        assert_eq!(post(None).await, StatusCode::FORBIDDEN);
        assert_eq!(
            post(Some("https://evil.example.com")).await,
            StatusCode::FORBIDDEN
        );
        assert!(stored_metrics(db.as_ref()).await.is_empty());

        assert_eq!(
            post(Some("https://app.example.com")).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(stored_metrics(db.as_ref()).await.len(), 1);

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn speed_key_over_its_rate_limit_returns_429() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_db_project(db.as_ref()).await;
        let state = build_state(db.clone());

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
                        .uri("/_temps/speed")
                        .header("host", "app.example.com")
                        .header("content-type", "application/json")
                        .header("x-temps-analytics-key", public_key)
                        .body(Body::from(speed_payload().to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
            }
        };

        assert_eq!(post().await, StatusCode::NO_CONTENT);
        assert_eq!(post().await, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(stored_metrics(db.as_ref()).await.len(), 1);

        test_db.cleanup().await;
    }

    /// Regression: no key at all resolves from Host exactly as before.
    #[tokio::test]
    async fn no_key_still_resolves_the_speed_scope_from_host() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let project = insert_db_project(db.as_ref()).await;
        let state = build_state(db.clone());
        insert_test_route(
            &state.route_table,
            "app.example.test",
            Some(project.clone()),
        );

        let response = setup_public_app(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_temps/speed")
                    .header("host", "app.example.test")
                    .header("content-type", "application/json")
                    .body(Body::from(speed_payload().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let metrics = stored_metrics(db.as_ref()).await;
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].project_id, project.id);
        assert_eq!(metrics[0].host.as_deref(), Some("app.example.test"));

        test_db.cleanup().await;
    }

    /// Regression: the no-key rejection shapes on both routes are unchanged —
    /// empty Host is 400, unknown host is 404, orphan route is a silent 204.
    #[tokio::test]
    async fn no_key_speed_rejection_shapes_are_unchanged() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let state = build_state(db.clone());
        insert_test_route(&state.route_table, "orphan.example.test", None);

        for uri in ["/_temps/speed", "/_temps/speed/update"] {
            for (host, expected) in [
                ("", StatusCode::BAD_REQUEST),
                ("unknown.example.test", StatusCode::NOT_FOUND),
                ("orphan.example.test", StatusCode::NO_CONTENT),
            ] {
                let response = setup_public_app(state.clone())
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri(uri)
                            .header("host", host)
                            .header("content-type", "application/json")
                            .body(Body::from(speed_payload().to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();

                assert_eq!(response.status(), expected, "{uri} with host {host:?}");
            }
        }

        assert!(stored_metrics(db.as_ref()).await.is_empty());

        test_db.cleanup().await;
    }

    #[tokio::test]
    async fn public_speed_routes_answer_cors_preflight_without_credentials() {
        let mut test_db: TestDatabase = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) => {
                println!("Database not available, skipping test: {}", e);
                return;
            }
        };
        let db = test_db.connection_arc();
        let state = build_state(db.clone());

        let response = setup_public_app(state)
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/_temps/speed")
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
