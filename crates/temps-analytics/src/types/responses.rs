use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct ReferrerCount {
    pub referrer: String,
    pub count: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PathVisitors {
    pub name: String,
    pub visitors: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PathVisitorsResponse {
    pub results: Vec<PathVisitors>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ViewItem {
    #[schema(value_type = String, format = DateTime)]
    pub label: UtcDateTime,
    pub value: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ViewsOverTime {
    pub items: Vec<ViewItem>,
    pub metric: String,
    pub comparison_labels: Option<Vec<String>>,
    pub comparison_plot: Option<Vec<i64>>,
    pub full_intervals: Option<Vec<String>>,
    pub present_index: usize,
}

#[derive(Serialize, ToSchema)]
pub struct AnalyticsMetrics {
    pub unique_visitors: i64,
    pub total_visits: i64,
    pub total_page_views: i64,
    pub views_per_visit: f64,
    pub average_visit_duration: f64,
    pub bounce_rate: f64,
    pub engagement_rate: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LocationCount {
    pub location: String,
    pub count: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BrowserCount {
    pub browser: String,
    pub count: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OperatingSystemCount {
    pub operating_system: String,
    pub count: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeviceCount {
    pub device_type: String,
    pub count: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StatusCodeCount {
    pub status_code: i32,
    pub count: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EventCount {
    pub event_name: String,
    pub count: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectStats {
    pub unique_visitors: i64,
    pub total_visits: i64,
    pub page_views: i64,
    pub bounce_rate: Option<f64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CountryStats {
    pub country: String,
    pub visitors: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeviceStats {
    pub device_type: String,
    pub visitors: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BrowserStats {
    pub browser: String,
    pub visitors: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VisitorInfo {
    pub id: i32,
    pub visitor_id: String,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub first_seen: UtcDateTime,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub last_seen: UtcDateTime,
    pub user_agent: Option<String>,
    pub location: Option<String>,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
    pub sessions_count: i64,
    pub page_views: i64,
    pub total_time_seconds: i64,
    pub unique_pages: i64,
    pub browser: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VisitorsResponse {
    pub visitors: Vec<VisitorInfo>,
    pub total_count: i64,
    pub filtered_count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VisitorDetails {
    pub id: i32,
    pub visitor_id: String,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub first_seen: UtcDateTime,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub last_seen: UtcDateTime,
    pub user_agent: Option<String>,
    pub location: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
    pub total_sessions: i64,
    pub total_page_views: i64,
    pub total_events: i64,
    pub total_time_seconds: i64,
    pub bounce_rate: f64,
    pub engagement_rate: f64,
    pub custom_data: Option<serde_json::Value>, // User-provided custom data
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VisitorRecord {
    pub id: i32,
    pub visitor_id: String,
    pub project_id: i32,
    pub custom_data: Option<serde_json::Value>,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub created_at: UtcDateTime,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VisitorStats {
    pub visitor_id: i32,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub first_seen: UtcDateTime,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub last_seen: UtcDateTime,
    pub total_sessions: i64,
    pub total_page_views: i64,
    pub total_events: i64,
    pub total_time_seconds: i64,
    pub average_session_duration: f64,
    pub bounce_rate: f64,
    pub engagement_rate: f64,
    pub top_pages: Vec<PageVisit>,
    pub top_referrers: Vec<String>,
    pub devices_used: Vec<String>,
    pub locations: Vec<LocationInfo>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PageVisit {
    pub path: String,
    pub visits: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LocationInfo {
    pub country: Option<String>,
    pub city: Option<String>,
    pub region: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionSummary {
    pub session_id: i32,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub started_at: UtcDateTime,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-01-01T00:00:00")]
    pub ended_at: Option<UtcDateTime>,
    pub duration_seconds: i64,
    pub page_views: i64,
    pub events_count: i64,
    pub requests_count: i64,
    pub entry_path: Option<String>,
    pub exit_path: Option<String>,
    pub referrer: Option<String>,
    pub is_bounced: bool,
    pub is_engaged: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VisitorSessionsResponse {
    pub visitor_id: String,
    pub sessions: Vec<SessionSummary>,
    pub total_sessions: i64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SessionEvent {
    pub id: i32,
    pub event_name: String,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub occurred_at: UtcDateTime,
    pub event_data: serde_json::Value,
    pub request_path: String,
    pub request_query: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SessionRequestLog {
    pub id: i32,
    pub method: String,
    pub path: String,
    pub status_code: i16,
    pub response_time_ms: Option<i32>,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub created_at: UtcDateTime,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub response_headers: Option<String>,
    pub request_headers: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionDetails {
    pub session_id: i32,
    pub visitor_id: String,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub started_at: UtcDateTime,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-01-01T00:00:00")]
    pub ended_at: Option<UtcDateTime>,
    pub duration_seconds: i64,
    pub entry_path: Option<String>,
    pub exit_path: Option<String>,
    pub referrer: Option<String>,
    pub is_bounced: bool,
    pub is_engaged: bool,
    pub page_views: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionEventsResponse {
    pub session_id: i32,
    pub events: Vec<SessionEvent>,
    pub total_count: i64,
    pub offset: i32,
    pub limit: i32,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionLogsResponse {
    pub session_id: i32,
    pub logs: Vec<SessionRequestLog>,
    pub total_count: i64,
    pub offset: i32,
    pub limit: i32,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnrichVisitorResponse {
    pub success: bool,
    pub visitor_id: String,
    pub message: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HasAnalyticsEventsResponse {
    pub has_events: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PageSessionStats {
    pub page_path: String,
    pub total_sessions: i64,
    pub total_time_seconds: f64,
    pub avg_time_seconds: f64,
    pub min_time_seconds: f64,
    pub max_time_seconds: f64,
    pub total_page_views: i64,
    pub avg_page_views_per_session: f64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PagePathInfo {
    pub page_path: String,
    pub session_count: i64,
    pub page_view_count: i64,
    pub avg_time_seconds: Option<f64>,
    #[schema(value_type = String)]
    pub first_seen: UtcDateTime,
    #[schema(value_type = String)]
    pub last_seen: UtcDateTime,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PagePathsResponse {
    pub page_paths: Vec<PagePathInfo>,
    pub total_count: usize,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ActiveVisitor {
    pub session_id: String,
    pub visitor_id: Option<String>,
    #[schema(value_type = String)]
    pub session_start: UtcDateTime,
    #[schema(value_type = String)]
    pub last_activity: UtcDateTime,
    pub page_count: i32,
    pub event_count: i32,
    pub current_page: Option<String>,
    pub duration_seconds: i64,
    pub is_active: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ActiveVisitorsResponse {
    pub count: i64,
    pub visitors: Vec<ActiveVisitor>,
    pub window_minutes: i32,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HourlyPageSessions {
    pub timestamp: String,
    pub session_count: i64,
    pub event_count: i64,
    pub avg_duration_seconds: f64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PageHourlySessionsResponse {
    pub page_path: String,
    pub hourly_data: Vec<HourlyPageSessions>,
    pub total_sessions: i64,
    pub hours: i32,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PageSessionComparison {
    pub page_path: String,
    #[schema(value_type = String)]
    pub date: chrono::NaiveDate,
    pub session_count: i64,
    pub event_count: i64,
    pub avg_duration_seconds: f64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PagesComparisonResponse {
    pub comparisons: Vec<PageSessionComparison>,
    pub page_paths: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VisitorWithGeolocation {
    pub id: i32,
    pub visitor_id: String,
    pub project_id: i32,
    pub environment_id: i32,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub first_seen: UtcDateTime,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub last_seen: UtcDateTime,
    pub user_agent: Option<String>,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
    pub custom_data: Option<serde_json::Value>,
    // Geolocation fields
    pub ip_address: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub region: Option<String>,
    pub city: Option<String>,
    pub country: Option<String>,
    pub country_code: Option<String>,
    pub timezone: Option<String>,
    pub is_eu: Option<bool>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GeneralStatsResponse {
    pub total_unique_visitors: i64,
    pub total_visits: i64,
    pub total_page_views: i64,
    pub total_events: i64,
    pub total_projects: i64,
    pub avg_bounce_rate: f64,
    pub avg_engagement_rate: f64,
    pub project_breakdown: Vec<ProjectStatsBreakdown>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectStatsBreakdown {
    pub project_id: i32,
    pub project_name: Option<String>,
    pub unique_visitors: i64,
    pub total_visits: i64,
    pub total_page_views: i64,
    pub bounce_rate: f64,
    pub engagement_rate: f64,
}