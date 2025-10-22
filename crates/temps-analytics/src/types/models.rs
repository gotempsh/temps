use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use temps_core::UtcDateTime;

#[derive(Debug, FromQueryResult)]
pub struct SelectCountResult {
    pub session_id: i32,
    pub count: i32,
}

#[derive(Debug, Serialize, FromQueryResult)]
pub struct ReferrerCount {
    pub referrer: String,
    pub count: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize)]
pub struct ViewItem {
    pub label: UtcDateTime,
    pub value: i64,
}

#[derive(Debug, Serialize)]
pub struct ViewsOverTime {
    pub items: Vec<ViewItem>,
    pub metric: String,
    pub comparison_items: Option<Vec<ViewItem>>,
    pub full_intervals: Option<Vec<String>>,
    pub present_index: usize,
}

#[derive(FromQueryResult, Serialize)]
pub struct DateCount {
    pub bucket: UtcDateTime,
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct AnalyticsData {
    pub metrics: HashMap<String, i64>,
    pub time_series: Option<Vec<ViewItem>>,
    pub breakdown: Option<HashMap<String, i64>>,
}

#[derive(Debug, Serialize, FromQueryResult)]
pub struct VisitorInfo {
    pub id: i32,
    pub visitor_id: String,
    pub first_seen: UtcDateTime,
    pub last_seen: UtcDateTime,
    pub user_agent: Option<String>,
    pub location: Option<String>,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
    pub sessions_count: i64,
    pub page_views: i64,
    pub total_time_seconds: i64,
}

#[derive(Debug, Serialize)]
pub struct VisitorsResponse {
    pub visitors: Vec<VisitorInfo>,
    pub total_count: i64,
    pub filtered_count: i64,
}

#[derive(Debug, Serialize, FromQueryResult)]
pub struct SessionSummaryResult {
    pub session_id: i32,
    pub started_at: UtcDateTime,
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

#[derive(Debug, Serialize, FromQueryResult)]
pub struct SessionEventResult {
    pub id: i32,
    pub event_name: String,
    pub occurred_at: UtcDateTime,
    pub event_data: String, // JSON string that will be parsed
    pub request_path: String,
    pub request_query: Option<String>,
}

#[derive(Debug, Serialize, FromQueryResult)]
pub struct SessionRequestLogResult {
    pub id: i32,
    pub method: String,
    pub request_path: String,
    pub status_code: i16,
    pub elapsed_time: Option<i32>,
    pub started_at: UtcDateTime,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub headers: Option<String>,
    pub request_headers: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionWithPageMetrics {
    pub session_id: String,
    pub visitor_id: Option<String>,
    pub session_start: UtcDateTime,
    pub session_end: UtcDateTime,
    pub total_duration_seconds: i64,
    pub page_count: i64,
    pub page_metrics: Vec<PageTimeMetric>,
    pub total_pageviews: i64,
    pub entry_page: String,
    pub exit_page: String,
    pub is_bounce: bool,
    pub avg_time_per_page: f64,
}

#[derive(Debug, Serialize, FromQueryResult)]
pub struct SessionMetricsResult {
    pub session_id: String,
    pub visitor_id: Option<String>,
    pub session_start: UtcDateTime,
    pub session_end: UtcDateTime,
    pub total_duration_seconds: i64,
    pub page_count: i64,
    pub page_paths: String,      // JSON array of page paths
    pub page_timestamps: String, // JSON array of timestamps
    pub time_on_pages: String,   // JSON array of durations
}

#[derive(Debug, Serialize)]
pub struct PageTimeMetric {
    pub page_path: String,
    pub timestamp: UtcDateTime,
    pub time_on_page_seconds: Option<i64>,
    pub is_exit_page: bool,
}

#[derive(Debug, Serialize, FromQueryResult)]
pub struct PageSessionMetrics {
    pub page_path: String,
    pub avg_duration_seconds: f64,
    pub total_duration_seconds: i64,
    pub session_count: i64,
    pub bounce_rate: f64,
    pub avg_pages_per_session: Option<f64>,
    pub median_duration: Option<f64>,
    pub view_count: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum PageDurationInterpretation {
    EntryPage,        // Duration when page is the entry page
    TimeOnPage,       // Actual time spent on the page
    SessionsWithPage, // Duration of sessions that viewed this page
}
