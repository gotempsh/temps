use super::types::AppState;
use crate::services::service::{
    GroupBy, GroupedPageMetric, GroupedPageMetricsResponse, MetricsOverTimeResponse,
    PerformanceMetricsResponse,
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
use temps_core::DateTime;
use tracing::{error, info};
use utoipa::{OpenApi, ToSchema};

#[derive(Deserialize, Clone, ToSchema)]
pub struct PerformanceMetricsQuery {
    start_date: DateTime,
    end_date: DateTime,
    project_id: i32,
    environment_id: Option<i32>,
    deployment_id: Option<i32>,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct GroupedPageMetricsQuery {
    start_date: DateTime,
    end_date: DateTime,
    project_id: i32,
    environment_id: Option<i32>,
    deployment_id: Option<i32>,
    group_by: String, // "path", "country", "device_type", "browser", "operating_system"
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

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/performance/metrics", get(get_performance_metrics))
        .route("/performance/metrics-over-time", get(get_metrics_over_time))
        .route("/performance/page-metrics", get(get_grouped_page_metrics))
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
        ("deployment_id" = Option<i32>, Query, description = "Deployment ID (optional)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved performance metrics", body = PerformanceMetricsResponse),
        (status = 400, description = "Invalid date format or missing parameters", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
async fn get_performance_metrics(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PerformanceMetricsQuery>,
) -> Result<Json<PerformanceMetricsResponse>, (StatusCode, Json<ErrorResponse>)> {
    match state
        .performance_service
        .get_metrics(
            query.start_date.into(),
            query.end_date.into(),
            query.project_id,
            query.environment_id,
            query.deployment_id,
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
        ("deployment_id" = Option<i32>, Query, description = "Deployment ID (optional)")
    ),
    responses(
        (status = 200, description = "Successfully retrieved metrics over time", body = MetricsOverTimeResponse),
        (status = 400, description = "Invalid date format or missing parameters", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
async fn get_metrics_over_time(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PerformanceMetricsQuery>,
) -> Result<Json<MetricsOverTimeResponse>, (StatusCode, Json<ErrorResponse>)> {
    match state
        .performance_service
        .get_metrics_over_time(
            query.start_date.into(),
            query.end_date.into(),
            query.project_id,
            query.environment_id,
            query.deployment_id,
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
        ("group_by" = String, Query, description = "Group by: path, country, device_type, browser, operating_system")
    ),
    responses(
        (status = 200, description = "Successfully retrieved grouped page metrics", body = GroupedPageMetricsResponse),
        (status = 400, description = "Invalid date format, missing parameters, or invalid group_by value", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
async fn get_grouped_page_metrics(
    State(state): State<Arc<AppState>>,
    Query(query): Query<GroupedPageMetricsQuery>,
) -> Result<Json<GroupedPageMetricsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let group_by = match query.group_by.as_str() {
        "path" => GroupBy::Path,
        "country" => GroupBy::Country,
        "device_type" => GroupBy::DeviceType,
        "browser" => GroupBy::Browser,
        "operating_system" => GroupBy::OperatingSystem,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Invalid group_by parameter".to_string(),
                    details: Some(
                        "group_by must be one of: path, country, device_type, browser, operating_system"
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

/// Record performance metrics from client
#[utoipa::path(
    tag = "Performance",
    post,
    path = "/_temps/speed",
    request_body = SpeedMetricsPayload,
    responses(
        (status = 200, description = "Metrics recorded successfully"),
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

    // Extract domain from Host header
    let host = match headers.get("host") {
        Some(host) => match host.to_str() {
            Ok(host_str) => host_str.to_string(),
            Err(_) => {
                error!("Invalid Host header");
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": "Invalid Host header"
                    })),
                )
                    .into_response();
            }
        },
        None => {
            error!("Missing Host header");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Missing Host header"
                })),
            )
                .into_response();
        }
    };

    // Look up project/environment/deployment from route table
    let (project_id, environment_id, deployment_id) = match state.route_table.get_route(&host) {
        Some(route_info) => {
            let project_id = route_info.project.as_ref().map(|p| p.id).unwrap_or(1);
            let environment_id = route_info.environment.as_ref().map(|e| e.id).unwrap_or(1);
            let deployment_id = route_info.deployment.as_ref().map(|d| d.id).unwrap_or(1);

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
        .record_performance_metrics(
            project_id,
            environment_id,
            deployment_id,
            metadata.session_id_cookie,
            metadata.visitor_id_cookie,
            ip_address_id,
            payload.ttfb,
            payload.lcp,
            payload.fid,
            payload.fcp,
            payload.cls,
            payload.inp,
            payload.pathname,
            payload.query,
            Some(host),
            user_agent,
            payload.screen_width,
            payload.screen_height,
            payload.viewport_width,
            payload.viewport_height,
            payload.language,
        )
        .await
    {
        Ok(_) => StatusCode::OK.into_response(),
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
        (status = 200, description = "Metrics updated successfully"),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 404, description = "Host not found or metrics not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn update_speed_metrics(
    State(state): State<Arc<AppState>>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    headers: HeaderMap,
    Json(payload): Json<UpdateSpeedMetricsPayload>,
) -> impl IntoResponse {
    info!("Updating late performance metrics from client");

    // Extract domain from Host header
    let host = match headers.get("host") {
        Some(host) => match host.to_str() {
            Ok(host_str) => host_str.to_string(),
            Err(_) => {
                error!("Invalid Host header");
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": "Invalid Host header"
                    })),
                )
                    .into_response();
            }
        },
        None => {
            error!("Missing Host header");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Missing Host header"
                })),
            )
                .into_response();
        }
    };

    // Look up project/environment/deployment from route table
    let (project_id, environment_id, deployment_id) = match state.route_table.get_route(&host) {
        Some(route_info) => {
            let project_id = route_info.project.as_ref().map(|p| p.id).unwrap_or(1);
            let environment_id = route_info.environment.as_ref().map(|e| e.id).unwrap_or(1);
            let deployment_id = route_info.deployment.as_ref().map(|d| d.id).unwrap_or(1);

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
        .update_performance_metrics(
            project_id,
            environment_id,
            deployment_id,
            metadata.session_id_cookie,
            metadata.visitor_id_cookie,
            payload.cls,
            payload.inp,
        )
        .await
    {
        Ok(_) => StatusCode::OK.into_response(),
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
