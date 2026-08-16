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
    pub project_id: i32,
    pub environment_id: i32,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub first_seen: UtcDateTime,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub last_seen: UtcDateTime,
    pub user_agent: Option<String>,
    pub ip_address_id: Option<i32>,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
    pub custom_data: Option<serde_json::Value>,
    // IP and Geolocation fields
    pub ip_address: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub region: Option<String>,
    pub city: Option<String>,
    pub country: Option<String>,
    pub country_code: Option<String>,
    pub timezone: Option<String>,
    pub is_eu: Option<bool>,
    /// Most recent page path visited by this visitor
    pub current_page: Option<String>,
    // First-visit attribution
    /// Full referrer URL from the visitor's first session
    pub first_referrer: Option<String>,
    /// Hostname extracted from first_referrer
    pub first_referrer_hostname: Option<String>,
    /// Marketing channel from the first visit (e.g. "Organic Search", "Direct")
    pub first_channel: Option<String>,
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
    pub project_id: i32,
    pub environment_id: i32,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub first_seen: UtcDateTime,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub last_seen: UtcDateTime,
    pub user_agent: Option<String>,
    pub ip_address_id: Option<i32>,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
    pub custom_data: Option<serde_json::Value>,
    // IP and Geolocation fields
    pub ip_address: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub region: Option<String>,
    pub city: Option<String>,
    pub country: Option<String>,
    pub country_code: Option<String>,
    pub timezone: Option<String>,
    pub is_eu: Option<bool>,
    // First-visit attribution
    /// Full referrer URL from the visitor's first session
    pub first_referrer: Option<String>,
    /// Hostname extracted from first_referrer
    pub first_referrer_hostname: Option<String>,
    /// Marketing channel from the first visit (e.g. "Organic Search", "Direct")
    pub first_channel: Option<String>,
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
pub struct PagePathSparklinePoint {
    pub timestamp: String,
    pub session_count: i64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PagePathSparkline {
    pub page_path: String,
    pub points: Vec<PagePathSparklinePoint>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PagePathsSparklineResponse {
    pub sparklines: Vec<PagePathSparkline>,
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
    // First-visit attribution
    /// Full referrer URL from the visitor's first session
    pub first_referrer: Option<String>,
    /// Hostname extracted from first_referrer
    pub first_referrer_hostname: Option<String>,
    /// Marketing channel from the first visit (e.g. "Organic Search", "Direct")
    pub first_channel: Option<String>,
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
    /// Previous period unique visitors (same duration, shifted back)
    pub previous_unique_visitors: Option<i64>,
    /// Previous period page views
    pub previous_page_views: Option<i64>,
    /// Percentage change in unique visitors vs previous period
    pub visitors_trend_percentage: Option<f64>,
    /// Percentage change in page views vs previous period
    pub page_views_trend_percentage: Option<f64>,
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

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LiveVisitorInfo {
    pub id: i32,
    pub visitor_id: String,
    pub project_id: i32,
    pub environment_id: i32,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub first_seen: UtcDateTime,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00")]
    pub last_seen: UtcDateTime,
    pub user_agent: Option<String>,
    pub ip_address_id: Option<i32>,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
    pub custom_data: Option<serde_json::Value>,
    // IP and Geolocation fields
    pub ip_address: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub region: Option<String>,
    pub city: Option<String>,
    pub country: Option<String>,
    pub country_code: Option<String>,
    pub timezone: Option<String>,
    pub is_eu: Option<bool>,
    /// Most recent page path visited by this visitor
    pub current_page: Option<String>,
    // First-visit attribution
    /// Full referrer URL from the visitor's first session
    pub first_referrer: Option<String>,
    /// Hostname extracted from first_referrer
    pub first_referrer_hostname: Option<String>,
    /// Marketing channel from the first visit (e.g. "Organic Search", "Direct")
    pub first_channel: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LiveVisitorsListResponse {
    pub total_count: i64,
    pub visitors: Vec<LiveVisitorInfo>,
    pub window_minutes: i32,
}

/// Time bucket data point for page activity graph
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct PageActivityBucket {
    /// Timestamp for this bucket (ISO 8601)
    #[schema(value_type = String)]
    pub timestamp: UtcDateTime,
    /// Number of unique visitors in this bucket
    pub visitors: i64,
    /// Number of page views in this bucket
    pub page_views: i64,
    /// Average time on page in seconds
    pub avg_time_seconds: f64,
}

/// Geographic distribution of visitors for a page
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct PageCountryStats {
    /// Country name
    pub country: String,
    /// ISO country code (2-letter)
    pub country_code: Option<String>,
    /// Number of unique visitors from this country
    pub visitors: i64,
    /// Number of page views from this country
    pub page_views: i64,
    /// Percentage of total visitors
    pub percentage: f64,
}

/// Referrer source for the page
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct PageReferrerStats {
    /// Referrer URL or domain
    pub referrer: String,
    /// Number of visits from this referrer
    pub visits: i64,
    /// Percentage of total visits
    pub percentage: f64,
}

/// Individual visitor session that viewed a specific page
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct PageVisitorSession {
    /// Visitor numeric ID
    pub visitor_id: i32,
    /// Visitor UUID
    pub visitor_uuid: String,
    /// Session ID
    pub session_id: Option<String>,
    /// When the page was viewed
    #[schema(value_type = String, format = "date-time")]
    pub viewed_at: UtcDateTime,
    /// Time spent on this page in seconds
    pub time_on_page: Option<i32>,
    /// Whether this was the entry page for the session
    pub is_entry: bool,
    /// Whether this was the exit page for the session
    pub is_exit: bool,
    /// Whether this was a bounce
    pub is_bounce: bool,
    /// Page number in session flow
    pub session_page_number: Option<i32>,
    /// Referrer URL
    pub referrer: Option<String>,
    /// Browser name
    pub browser: Option<String>,
    /// Operating system
    pub operating_system: Option<String>,
    /// Device type (Desktop, Mobile, Tablet)
    pub device_type: Option<String>,
    /// Visitor's city
    pub city: Option<String>,
    /// Visitor's country
    pub country: Option<String>,
    /// Visitor's country code
    pub country_code: Option<String>,
}

/// Response for page path visitors endpoint
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PagePathVisitorsResponse {
    /// The page path
    pub page_path: String,
    /// Total number of visitor sessions matching the query
    pub total_count: i64,
    /// Current page number
    pub page: u64,
    /// Items per page
    pub per_page: u64,
    /// Individual visitor sessions
    pub sessions: Vec<PageVisitorSession>,
}

/// Detailed analytics response for a specific page path
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PagePathDetailResponse {
    /// The page path being analyzed
    pub page_path: String,
    /// Total unique visitors to this page in the date range
    pub unique_visitors: i64,
    /// Total page views in the date range
    pub total_page_views: i64,
    /// Average time on page in seconds
    pub avg_time_on_page: f64,
    /// Bounce rate percentage (0-100)
    pub bounce_rate: f64,
    /// Entry rate - percentage of sessions that started on this page
    pub entry_rate: f64,
    /// Exit rate - percentage of sessions that ended on this page
    pub exit_rate: f64,
    /// Time series data for activity graph
    pub activity_over_time: Vec<PageActivityBucket>,
    /// Geographic distribution of visitors
    pub countries: Vec<PageCountryStats>,
    /// Top referrers to this page
    pub referrers: Vec<PageReferrerStats>,
    /// Bucket interval used for time series ('hour', 'day', etc.)
    pub bucket_interval: String,
}

// ============================================================================
// Visitor Journey types
// ============================================================================

/// A single event in the visitor journey timeline
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct JourneyEvent {
    /// Event ID
    pub id: i64,
    /// Event type: "page_view", "page_leave", "custom", "web_vitals"
    pub event_type: String,
    /// Resolved event name (event_name for custom events, event_type for system events)
    pub event_name: String,
    /// When the event occurred
    #[schema(value_type = String, format = "date-time")]
    pub occurred_at: UtcDateTime,
    /// Page path where the event happened
    pub page_path: Option<String>,
    /// Page title (if available)
    pub page_title: Option<String>,
    /// Referrer URL for this event
    pub referrer: Option<String>,
    /// Time spent on page in seconds (computed, not from column)
    pub time_on_page: Option<i32>,
    /// Whether this is the entry page of the session
    pub is_entry: bool,
    /// Whether this is the exit page of the session
    pub is_exit: bool,
    /// Whether this was a bounce
    pub is_bounce: bool,
    /// Page number within the session (1-indexed)
    pub session_page_number: Option<i32>,
    /// Scroll depth percentage (0-100)
    pub scroll_depth: Option<i32>,
    /// Custom event properties (for custom events)
    pub event_data: Option<serde_json::Value>,
}

/// A session within the visitor journey, grouping events
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct JourneySession {
    /// Session internal ID
    pub session_id: i32,
    /// When the session started
    #[schema(value_type = String, format = "date-time")]
    pub started_at: UtcDateTime,
    /// When the session ended
    #[schema(value_type = Option<String>, format = "date-time")]
    pub ended_at: Option<UtcDateTime>,
    /// Session duration in seconds
    pub duration_seconds: i64,
    /// Number of page views in this session
    pub page_views: i64,
    /// Total events in this session
    pub events_count: i64,
    /// Entry page path
    pub entry_path: Option<String>,
    /// Exit page path
    pub exit_path: Option<String>,
    /// Traffic source: referrer URL
    pub referrer: Option<String>,
    /// Traffic source: referrer hostname
    pub referrer_hostname: Option<String>,
    /// Traffic source: channel (e.g. "organic", "direct", "social")
    pub channel: Option<String>,
    /// UTM source parameter
    pub utm_source: Option<String>,
    /// UTM medium parameter
    pub utm_medium: Option<String>,
    /// UTM campaign parameter
    pub utm_campaign: Option<String>,
    /// Whether the session was a bounce
    pub is_bounced: bool,
    /// Whether the visitor was engaged (had non-pageview events)
    pub is_engaged: bool,
    /// Events within this session, ordered chronologically
    pub events: Vec<JourneyEvent>,
}

/// Complete visitor journey response
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct VisitorJourneyResponse {
    /// Visitor internal ID
    pub visitor_id: i32,
    /// Total number of sessions
    pub total_sessions: i64,
    /// Total number of events across all sessions
    pub total_events: i64,
    /// Sessions with their events, ordered newest first
    pub sessions: Vec<JourneySession>,
}

// ---- Page Flow / Journey Analytics types ----

/// A single page with its entry/exit/bounce statistics
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PageFlowEntry {
    /// The page path (e.g. "/pricing", "/docs/getting-started")
    pub page_path: String,
    /// Number of times this page was the entry page of a session
    pub entry_count: i64,
    /// Number of times this page was the exit page of a session
    pub exit_count: i64,
    /// Number of times visitors bounced on this page
    pub bounce_count: i64,
    /// Total page views for this page
    pub total_views: i64,
    /// Average time spent on this page in seconds
    pub avg_time_on_page: Option<f64>,
    /// Entry rate: entry_count / total_views
    pub entry_rate: f64,
    /// Exit rate: exit_count / total_views
    pub exit_rate: f64,
    /// Bounce rate: bounce_count / entry_count (only meaningful for entry pages)
    pub bounce_rate: f64,
}

/// A page-to-page transition with count
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PageTransition {
    /// The source page path
    pub from_page: String,
    /// The destination page path
    pub to_page: String,
    /// Number of times this transition occurred
    pub transition_count: i64,
    /// Percentage of transitions from the source page that go to this destination
    pub percentage: f64,
}

/// Drop-off point: pages where visitors leave the site
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DropOffPoint {
    /// The page path where visitors drop off
    pub page_path: String,
    /// Number of exits from this page
    pub exit_count: i64,
    /// Total views of this page
    pub total_views: i64,
    /// Exit rate for this page (exit_count / total_views)
    pub exit_rate: f64,
}

/// A single activity event for the real-time activity feed
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct ActivityEvent {
    /// Event ID
    pub id: i64,
    /// When the event occurred
    #[schema(value_type = String, format = "date-time")]
    pub timestamp: UtcDateTime,
    /// Event type: "page_view", "custom", etc.
    pub event_type: String,
    /// Event name (for custom events)
    pub event_name: Option<String>,
    /// Page path where the event happened
    pub page_path: String,
    /// Page title
    pub page_title: Option<String>,
    /// Visitor numeric ID
    pub visitor_id: Option<i32>,
    /// Browser
    pub browser: Option<String>,
    /// Operating system
    pub operating_system: Option<String>,
    /// Device type
    pub device_type: Option<String>,
    /// Referrer
    pub referrer: Option<String>,
    /// Visitor's city (from ip_geolocations)
    pub city: Option<String>,
    /// Visitor's country (from ip_geolocations)
    pub country: Option<String>,
    /// Visitor's country code (from ip_geolocations)
    pub country_code: Option<String>,
    /// Latitude
    pub latitude: Option<f64>,
    /// Longitude
    pub longitude: Option<f64>,
    /// Whether this event was from a crawler
    pub is_crawler: bool,
}

/// Response for recent activity events endpoint
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RecentActivityResponse {
    /// Recent events, newest first
    pub events: Vec<ActivityEvent>,
    /// Total events returned
    pub count: usize,
}

// ============================================================================
// Event Detail types
// ============================================================================

/// Summary response for a specific event's analytics
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EventDetailResponse {
    /// The event name being analyzed
    pub event_name: String,
    /// Total number of times this event was triggered in the date range
    pub total_count: i64,
    /// Number of unique visitors who triggered this event
    pub unique_visitors: i64,
    /// Number of unique sessions where this event occurred
    pub unique_sessions: i64,
    /// Time series data for event activity graph
    pub activity_over_time: Vec<EventActivityBucket>,
    /// Top referrer hostnames for visitors who triggered this event
    pub referrers: Vec<EventReferrerStats>,
    /// Geographic distribution of visitors who triggered this event
    pub countries: Vec<EventCountryStats>,
    /// Browser distribution of visitors who triggered this event
    pub browsers: Vec<EventBrowserStats>,
    /// Bucket interval used for time series ('hour', 'day', etc.)
    pub bucket_interval: String,
}

/// Time bucket data point for event activity graph
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct EventActivityBucket {
    /// Timestamp for this bucket (ISO 8601)
    #[schema(value_type = String)]
    pub timestamp: UtcDateTime,
    /// Number of event occurrences in this bucket
    pub count: i64,
    /// Number of unique visitors in this bucket
    pub unique_visitors: i64,
}

/// Referrer stats for an event
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct EventReferrerStats {
    /// Referrer hostname or "Direct"
    pub referrer: String,
    /// Number of event occurrences from this referrer
    pub count: i64,
    /// Percentage of total events
    pub percentage: f64,
}

/// Country stats for an event
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct EventCountryStats {
    /// Country name
    pub country: String,
    /// ISO country code (2-letter)
    pub country_code: Option<String>,
    /// Number of event occurrences from this country
    pub count: i64,
    /// Percentage of total events
    pub percentage: f64,
}

/// Browser stats for an event
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct EventBrowserStats {
    /// Browser name
    pub browser: String,
    /// Number of event occurrences from this browser
    pub count: i64,
    /// Percentage of total events
    pub percentage: f64,
}

/// A visitor who triggered a specific event
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct EventVisitorInfo {
    /// Visitor numeric ID
    pub visitor_id: i32,
    /// Visitor UUID
    pub visitor_uuid: String,
    /// Number of times this visitor triggered the event
    pub event_count: i64,
    /// When the visitor first triggered the event in the date range
    #[schema(value_type = String, format = "date-time")]
    pub first_triggered: UtcDateTime,
    /// When the visitor last triggered the event in the date range
    #[schema(value_type = String, format = "date-time")]
    pub last_triggered: UtcDateTime,
    /// Visitor's country
    pub country: Option<String>,
    /// Visitor's country code
    pub country_code: Option<String>,
    /// Visitor's city
    pub city: Option<String>,
    /// Browser name
    pub browser: Option<String>,
    /// Device type (Desktop, Mobile, Tablet)
    pub device_type: Option<String>,
    /// Referrer hostname for the event
    pub referrer_hostname: Option<String>,
}

/// Paginated response for event visitors
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EventVisitorsResponse {
    /// The event name
    pub event_name: String,
    /// Total number of unique visitors who triggered this event
    pub total_count: i64,
    /// Current page number
    pub page: u64,
    /// Items per page
    pub per_page: u64,
    /// Individual visitors who triggered this event
    pub visitors: Vec<EventVisitorInfo>,
}

/// A single raw occurrence of an event, including its custom JSON properties
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct EventEntryInfo {
    /// Event row ID
    pub id: i64,
    /// When the event occurred
    #[schema(value_type = String, format = "date-time")]
    pub timestamp: UtcDateTime,
    /// Visitor numeric ID (if known)
    pub visitor_id: Option<i32>,
    /// Visitor UUID (if known)
    pub visitor_uuid: Option<String>,
    /// Session ID the event belongs to (if any)
    pub session_id: Option<String>,
    /// Page path where the event was triggered
    pub page_path: String,
    /// Full URL where the event was triggered
    pub href: String,
    /// Browser name
    pub browser: Option<String>,
    /// Device type (Desktop, Mobile, Tablet)
    pub device_type: Option<String>,
    /// Country of the visitor at the time of the event
    pub country: Option<String>,
    /// ISO country code (2-letter)
    pub country_code: Option<String>,
    /// City of the visitor at the time of the event
    pub city: Option<String>,
    /// Custom event properties as JSON (null when the event carried no data)
    #[schema(value_type = Option<Object>)]
    pub props: Option<serde_json::Value>,
}

/// Paginated response for raw event entries
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EventEntriesResponse {
    /// The event name
    pub event_name: String,
    /// Total number of occurrences of this event in the date range
    pub total_count: i64,
    /// Current page number
    pub page: u64,
    /// Items per page
    pub per_page: u64,
    /// Individual event occurrences, most recent first
    pub entries: Vec<EventEntryInfo>,
}

/// Complete page flow analytics response
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PageFlowResponse {
    /// Top entry pages (where visitors land), sorted by entry_count DESC
    pub top_entry_pages: Vec<PageFlowEntry>,
    /// Top exit pages (where visitors leave), sorted by exit_count DESC
    pub top_exit_pages: Vec<PageFlowEntry>,
    /// Top drop-off points (highest exit rates with meaningful traffic)
    pub drop_off_points: Vec<DropOffPoint>,
    /// Page-to-page transitions (most common navigation paths)
    pub transitions: Vec<PageTransition>,
    /// Total unique pages seen in the period
    pub total_pages: i64,
    /// Total sessions in the period
    pub total_sessions: i64,
}

/// A single facet value with its visitor count. Used to populate filter
/// dropdowns on the visitors page (e.g. "Germany — 1,234 visitors").
#[derive(Debug, Serialize, ToSchema)]
pub struct VisitorFacetValue {
    /// The dimension value (e.g. "United States", "Chrome", "google.com").
    /// `None` is encoded as the literal string "Direct" for referrer and as
    /// the empty string for the rest.
    pub value: String,
    /// Optional secondary code for the value. Currently only populated for
    /// the `country` facet, where it carries the 2-letter ISO country code
    /// so the UI can render a flag without re-mapping.
    pub code: Option<String>,
    /// Distinct visitor count matching this value in the current segment.
    pub count: i64,
}

/// All filter dropdown contents in one response. Each list is the top N
/// values for that dimension within the current date range and segment
/// (excluding the dimension being queried so the dropdown still shows
/// alternatives when a value is already selected).
#[derive(Debug, Serialize, ToSchema, Default)]
pub struct VisitorFacets {
    pub country: Vec<VisitorFacetValue>,
    pub region: Vec<VisitorFacetValue>,
    pub city: Vec<VisitorFacetValue>,
    pub channel: Vec<VisitorFacetValue>,
    pub referrer: Vec<VisitorFacetValue>,
}
