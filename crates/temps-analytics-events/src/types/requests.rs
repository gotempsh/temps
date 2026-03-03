use serde::Deserialize;
use temps_core::DateTime;
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AggregationLevel {
    /// Raw event counts - total number of events fired
    #[default]
    Events,
    /// Unique sessions - count distinct sessions that fired the event
    Sessions,
    /// Unique visitors - count distinct visitors who fired the event
    Visitors,
}

impl AggregationLevel {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Events => "events",
            Self::Sessions => "sessions",
            Self::Visitors => "visitors",
        }
    }
}

#[derive(Debug, Deserialize, ToSchema, Clone)]
#[serde(rename_all = "snake_case")]
pub enum PropertyColumn {
    /// Traffic source channel (direct, organic, paid, social, etc.)
    Channel,
    /// Device type (desktop, mobile, tablet)
    DeviceType,
    /// Browser name (Chrome, Firefox, Safari, etc.)
    Browser,
    /// Browser version
    BrowserVersion,
    /// Operating system (Windows, macOS, Linux, iOS, Android, etc.)
    OperatingSystem,
    /// Operating system version
    OperatingSystemVersion,
    /// UTM source parameter
    UtmSource,
    /// UTM medium parameter
    UtmMedium,
    /// UTM campaign parameter
    UtmCampaign,
    /// UTM term parameter
    UtmTerm,
    /// UTM content parameter
    UtmContent,
    /// Referrer hostname
    ReferrerHostname,
    /// Visitor language
    Language,
    /// Event type
    EventType,
    /// Event name
    EventName,
    /// Page path (full path)
    PagePath,
    /// Pathname (path without query string)
    Pathname,
    /// Visitor country (from IP geolocation)
    Country,
    /// Visitor region/state (from IP geolocation)
    Region,
    /// Visitor city (from IP geolocation)
    City,
}

impl PropertyColumn {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Channel => "channel",
            Self::DeviceType => "device_type",
            Self::Browser => "browser",
            Self::BrowserVersion => "browser_version",
            Self::OperatingSystem => "operating_system",
            Self::OperatingSystemVersion => "operating_system_version",
            Self::UtmSource => "utm_source",
            Self::UtmMedium => "utm_medium",
            Self::UtmCampaign => "utm_campaign",
            Self::UtmTerm => "utm_term",
            Self::UtmContent => "utm_content",
            Self::ReferrerHostname => "referrer_hostname",
            Self::Language => "language",
            Self::EventType => "event_type",
            Self::EventName => "event_name",
            Self::PagePath => "page_path",
            Self::Pathname => "pathname",
            Self::Country => "country",
            Self::Region => "region",
            Self::City => "city",
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EventsCountQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub environment_id: Option<i32>,
    pub limit: Option<i32>,
    /// Only return custom events, excluding system events like page_view, page_leave, heartbeat (default: true)
    pub custom_events_only: Option<bool>,
    /// Aggregation level: events (raw count), sessions (unique sessions), or visitors (unique visitors)
    #[serde(default)]
    pub aggregation_level: AggregationLevel,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct HasEventsQuery {
    pub project_id: i32,
    pub environment_id: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SessionEventsQuery {
    pub project_id: i32,
    pub environment_id: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EventTypeBreakdownQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub environment_id: Option<i32>,
    /// Aggregation level: events (raw count), sessions (unique sessions), or visitors (unique visitors)
    #[serde(default)]
    pub aggregation_level: AggregationLevel,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EventTimelineQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub environment_id: Option<i32>,
    pub event_name: Option<String>,
    /// Bucket size: hour, day, or week (auto-detected if not specified)
    pub bucket_size: Option<String>,
    /// Aggregation level: events (raw count), sessions (unique sessions), or visitors (unique visitors)
    #[serde(default)]
    pub aggregation_level: AggregationLevel,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EventPropertiesQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub environment_id: Option<i32>,
    pub event_name: String,
    pub property_path: String,
    pub limit: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ActiveVisitorsQuery {
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct HourlyVisitsQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub environment_id: Option<i32>,
    /// Aggregation level: events (page views), sessions (unique sessions), or visitors (unique visitors)
    #[serde(default)]
    pub aggregation_level: AggregationLevel,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EventMetricsPayload {
    pub event_name: String,
    pub event_data: serde_json::Value,
    pub request_path: String,
    pub request_query: String,
    pub screen_width: Option<u32>,
    pub screen_height: Option<u32>,
    pub viewport_width: Option<u32>,
    pub viewport_height: Option<u32>,
    pub language: Option<String>,
    pub page_title: Option<String>,
    /// Referrer URL (falls back to Referer header if not provided)
    pub referrer: Option<String>,
    // Performance metrics (web vitals) - optional
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
}

/// Payload for server-side event ingestion via the console API.
///
/// The app backend reads the encrypted `_temps_visitor_id` and `_temps_sid`
/// cookie values from the user's request and forwards them here.
/// Temps decrypts them server-side to resolve visitor/session identity.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ConsoleEventPayload {
    /// Event name (e.g. "purchase", "signup", custom event names)
    pub event_name: String,
    /// Arbitrary JSON event data
    #[serde(default = "default_event_data")]
    pub event_data: serde_json::Value,
    /// Encrypted `_temps_visitor_id` cookie value from the user's browser
    pub visitor_id: Option<String>,
    /// Encrypted `_temps_sid` cookie value from the user's browser
    pub session_id: Option<String>,
    /// Environment ID to attribute the event to
    pub environment_id: i32,
    /// Deployment ID to attribute the event to
    pub deployment_id: i32,
    /// Page path context (defaults to "/")
    #[serde(default = "default_request_path")]
    pub request_path: String,
    /// Query string context
    #[serde(default)]
    pub request_query: String,
}

fn default_event_data() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn default_request_path() -> String {
    "/".to_string()
}

/// Query parameters for property breakdown (group by column)
#[derive(Debug, Deserialize, ToSchema)]
pub struct PropertyBreakdownQuery {
    /// Start date for the query range
    pub start_date: DateTime,
    /// End date for the query range
    pub end_date: DateTime,
    /// Optional environment filter
    pub environment_id: Option<i32>,
    /// Optional deployment filter
    pub deployment_id: Option<i32>,
    /// Optional event name filter (e.g., "page_view", "click")
    pub event_name: Option<String>,
    /// Property column to group by
    pub group_by: PropertyColumn,
    /// Aggregation level
    #[serde(default)]
    pub aggregation_level: AggregationLevel,
    /// Maximum number of results to return (default: 20, max: 100)
    pub limit: Option<i32>,
    /// Filter by country (for region/city drill-downs). Requires geolocation join.
    pub filter_country: Option<String>,
    /// Filter by region (for city drill-downs). Requires geolocation join.
    pub filter_region: Option<String>,
    /// Filter by browser name (for browser version drill-downs)
    pub filter_browser: Option<String>,
    /// Filter by operating system name (for OS version drill-downs)
    pub filter_os: Option<String>,
    /// Filter by channel name (for channel -> referrer drill-downs)
    pub filter_channel: Option<String>,
}

/// Optional filters for property breakdown drill-downs.
/// These allow hierarchical navigation (e.g., country -> region -> city).
#[derive(Debug, Default)]
pub struct PropertyBreakdownFilters {
    /// Filter by country name (for region/city drill-downs)
    pub country: Option<String>,
    /// Filter by region name (for city drill-downs)
    pub region: Option<String>,
    /// Filter by browser name (for version drill-downs)
    pub browser: Option<String>,
    /// Filter by operating system name (for version drill-downs)
    pub operating_system: Option<String>,
    /// Filter by channel name (for channel -> referrer drill-downs)
    pub channel: Option<String>,
}

/// Query parameters for property timeline (group by column over time)
#[derive(Debug, Deserialize, ToSchema)]
pub struct PropertyTimelineQuery {
    /// Start date for the query range
    pub start_date: DateTime,
    /// End date for the query range
    pub end_date: DateTime,
    /// Optional environment filter
    pub environment_id: Option<i32>,
    /// Optional deployment filter
    pub deployment_id: Option<i32>,
    /// Optional event name filter
    pub event_name: Option<String>,
    /// Property column to group by
    pub group_by: PropertyColumn,
    /// Aggregation level
    #[serde(default)]
    pub aggregation_level: AggregationLevel,
    /// Time bucket size: "hour", "day", "week", "month" (default: auto-detect)
    pub bucket_size: Option<String>,
}

/// Query parameters for unique counts over time frame
#[derive(Debug, Deserialize, ToSchema)]
pub struct UniqueCountsQuery {
    /// Start date for the query range
    pub start_date: DateTime,
    /// End date for the query range
    pub end_date: DateTime,
    /// Optional environment filter
    pub environment_id: Option<i32>,
    /// Optional deployment filter
    pub deployment_id: Option<i32>,
    /// Metric to count: "sessions" (unique sessions), "visitors" (unique visitors), or "page_views" (total page views) (default: "sessions")
    #[serde(default = "default_metric")]
    pub metric: String,
}

fn default_metric() -> String {
    "sessions".to_string()
}

/// Query parameters for aggregated metrics by time bucket
#[derive(Debug, Deserialize, ToSchema)]
pub struct AggregatedBucketsQuery {
    /// Start date for the query range
    pub start_date: DateTime,
    /// End date for the query range
    pub end_date: DateTime,
    /// Optional environment filter
    pub environment_id: Option<i32>,
    /// Optional deployment filter
    pub deployment_id: Option<i32>,
    /// Aggregation level: events, sessions, or visitors
    #[serde(default)]
    pub aggregation_level: AggregationLevel,
    /// Time bucket size: "1 hour", "1 day", "1 week", etc. (default: "1 hour")
    #[serde(default = "default_bucket_size")]
    pub bucket_size: String,
}

fn default_bucket_size() -> String {
    "1 hour".to_string()
}

/// Query parameters for batch dashboard analytics
#[derive(Debug, Deserialize, ToSchema)]
pub struct DashboardProjectsAnalyticsQuery {
    /// Comma-separated list of project IDs
    pub project_ids: String,
    /// Start date for the query range
    pub start_date: DateTime,
    /// End date for the query range
    pub end_date: DateTime,
}
