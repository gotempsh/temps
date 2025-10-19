use serde::{Deserialize};
use serde_json::Value;
use temps_core::DateTime;
use utoipa::ToSchema;

#[derive(Debug, Deserialize, Clone, ToSchema)]
pub enum LocationGranularity {
    #[serde(rename = "country")]
    Country,
    #[serde(rename = "region")]
    Region,
    #[serde(rename = "city")]
    City,
}

// Deprecated: Use specific query structs for each endpoint
#[derive(Deserialize, Clone, ToSchema)]
pub struct AnalyticsQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub limit: Option<i32>,
    pub granularity: Option<LocationGranularity>,
    pub include_crawlers: Option<bool>,
}

// Specific query structs for each endpoint
#[derive(Deserialize, Clone, ToSchema)]
pub struct MetricsQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct ViewsOverTimeQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct PathVisitorsAnalyticsQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub limit: Option<i32>,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct ReferrersAnalyticsQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub project_id: i32,
    pub environment_id: Option<i32>,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct VisitorLocationsQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub limit: Option<i32>,
    pub granularity: Option<LocationGranularity>,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct BrowsersQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub limit: Option<i32>,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct StatusCodesQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub limit: Option<i32>,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct EventsCountQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub limit: Option<i32>,
    pub custom_events_only: Option<bool>,
    pub breakdown: Option<EventBreakdown>,
}

#[derive(Debug, Deserialize, Clone, Copy, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventBreakdown {
    Country,
    Region,
    City,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct VisitorsListQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub include_crawlers: Option<bool>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
}


#[derive(Deserialize, Clone, ToSchema)]
pub struct VisitorSessionsQuery {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub limit: Option<i32>,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct SessionDetailsQuery {
    pub project_id: i32,
    pub environment_id: Option<i32>,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct SessionEventsQuery {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub start_date: Option<DateTime>,
    pub end_date: Option<DateTime>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
    pub sort_order: Option<String>, // "asc" or "desc", defaults to "desc"
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct SessionLogsQuery {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub start_date: Option<DateTime>,
    pub end_date: Option<DateTime>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
    pub sort_order: Option<String>, // "asc" or "desc", defaults to "desc"
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EventQuery {
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub event_name: Option<String>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ReferrerQuery {
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub limit: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PathVisitorsQuery {
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub limit: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct VisitorsQuery {
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub include_crawlers: Option<bool>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
    pub sort_by: Option<String>, // "first_seen", "last_seen", "sessions", "page_views"
    pub sort_order: Option<String>, // "asc", "desc"
}

// Metrics request types
#[derive(Deserialize, ToSchema)]
pub struct SpeedMetricsPayload {
    pub ttfb: Option<f32>,
    pub lcp: Option<f32>,
    pub fid: Option<f32>,
    pub fcp: Option<f32>,
    pub cls: Option<f32>,
    pub inp: Option<f32>,
    pub request_id: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct EventMetricsPayload {
    pub event_name: String,
    pub event_data: Value,
    pub request_path: String,
    pub request_query: String,
    pub screen_width: Option<i16>,
    pub screen_height: Option<i16>,
    pub viewport_width: Option<i16>,
    pub viewport_height: Option<i16>,
    pub language: Option<String>,
    pub page_title: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateSpeedMetricsPayload {
    pub request_id: Option<String>,
    pub cls: Option<f32>,
    pub inp: Option<f32>,
    pub session_id: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct EnrichVisitorRequest {
    #[schema(value_type = Object)]
    pub custom_data: serde_json::Value,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct ProjectQuery {
    pub project_id: i32,
    pub environment_id: Option<i32>,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct PageSessionStatsQuery {
    pub page_path: String,
    pub project_id: i32,
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub environment_id: Option<i32>,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct PagePathsQuery {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub start_date: Option<DateTime>,
    pub end_date: Option<DateTime>,
    pub limit: Option<i32>,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct GeneralStatsQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
}