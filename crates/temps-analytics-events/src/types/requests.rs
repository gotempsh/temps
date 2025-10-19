use serde::Deserialize;
use temps_core::DateTime;
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum AggregationLevel {
    /// Raw event counts - total number of events fired
    Events,
    /// Unique sessions - count distinct sessions that fired the event
    Sessions,
    /// Unique visitors - count distinct visitors who fired the event
    Visitors,
}

impl Default for AggregationLevel {
    fn default() -> Self {
        AggregationLevel::Events
    }
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
