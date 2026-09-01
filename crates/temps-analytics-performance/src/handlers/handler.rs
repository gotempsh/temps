// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::types::AppState;
use crate::services::service::{
    GroupBy, GroupedPageMetric, GroupedPageMetricsResponse, MetricsOverTimeResponse,
    PerformanceMetricsResponse, RecordPerformanceMetricsConfig, SpeedSegmentFilters,
    UpdatePerformanceMetricsConfig,
};
use axum::http::header::HeaderMap;
use axum::Extension;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{project_access_guard, AuthContext, Permission, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::DateTime;
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
}

/// Update speed metrics payload for late-loading metrics
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSpeedMetricsPayload {
    /// Cumulative Layout Shift (score)
    pub cls: Option<f32>,
    /// Interaction to Next Paint (milliseconds)
    pub inp: Option<f32>,
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

/// Record performance metrics from client
#[utoipa::path(
    tag = "Performance",
    post,
    path = "/_temps/speed",
    request_body = SpeedMetricsPayload,
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
    headers: HeaderMap,
    Json(payload): Json<SpeedMetricsPayload>,
) -> impl IntoResponse {
    info!("Recording speed metrics from client");

    // Host comes from `RequestMetadata`, which the auth middleware already
    // normalizes by stripping any ":port" suffix. That matches the proxy's
    // route-table keying so `get_route` works correctly even on non-default
    // ports (e.g. the :8080 dev proxy).
    let host = metadata.host.clone();
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
    let (project_id, environment_id, deployment_id) = match state.route_table.get_route(&host) {
        Some(route_info) => {
            let Some(project) = route_info.project.as_ref() else {
                info!(
                    "Dropping performance event for host {} — no associated project",
                    host
                );
                return StatusCode::NO_CONTENT.into_response();
            };
            let Some(environment) = route_info.environment.as_ref() else {
                info!(
                    "Dropping performance event for host {} — no associated environment",
                    host
                );
                return StatusCode::NO_CONTENT.into_response();
            };
            let Some(deployment) = route_info.deployment.as_ref() else {
                info!(
                    "Dropping performance event for host {} — no associated deployment",
                    host
                );
                return StatusCode::NO_CONTENT.into_response();
            };
            let project_id = project.id;
            let environment_id = environment.id;
            let deployment_id = deployment.id;

            info!(
                "Resolved host {} to project={}, env={}, deploy={}",
                host, project_id, environment_id, deployment_id
            );

            (project_id, environment_id, deployment_id)
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

    match state
        .performance_service
        .record_performance_metrics(RecordPerformanceMetricsConfig {
            project_id,
            environment_id,
            deployment_id,
            session_id: metadata.session_id_cookie,
            visitor_id: metadata.visitor_id_cookie,
            ip_address_id,
            ttfb: payload.ttfb,
            lcp: payload.lcp,
            fid: payload.fid,
            fcp: payload.fcp,
            cls: payload.cls,
            inp: payload.inp,
            pathname: payload.pathname,
            query: payload.query,
            host: Some(host),
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
    Json(payload): Json<UpdateSpeedMetricsPayload>,
) -> impl IntoResponse {
    info!("Updating late performance metrics from client");

    // Host comes from `RequestMetadata`, which the auth middleware already
    // normalizes by stripping any ":port" suffix. That matches the proxy's
    // route-table keying so `get_route` works correctly even on non-default
    // ports (e.g. the :8080 dev proxy).
    let host = metadata.host.clone();
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
    let (project_id, environment_id, deployment_id) = match state.route_table.get_route(&host) {
        Some(route_info) => {
            let Some(project) = route_info.project.as_ref() else {
                info!(
                    "Dropping performance update for host {} — no associated project",
                    host
                );
                return StatusCode::NO_CONTENT.into_response();
            };
            let Some(environment) = route_info.environment.as_ref() else {
                info!(
                    "Dropping performance update for host {} — no associated environment",
                    host
                );
                return StatusCode::NO_CONTENT.into_response();
            };
            let Some(deployment) = route_info.deployment.as_ref() else {
                info!(
                    "Dropping performance update for host {} — no associated deployment",
                    host
                );
                return StatusCode::NO_CONTENT.into_response();
            };
            let project_id = project.id;
            let environment_id = environment.id;
            let deployment_id = deployment.id;

            info!(
                "Resolved host {} to project={}, env={}, deploy={}",
                host, project_id, environment_id, deployment_id
            );

            (project_id, environment_id, deployment_id)
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
    };

    match state
        .performance_service
        .update_performance_metrics(UpdatePerformanceMetricsConfig {
            project_id,
            environment_id,
            deployment_id,
            session_id: metadata.session_id_cookie,
            visitor_id: metadata.visitor_id_cookie,
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
}
