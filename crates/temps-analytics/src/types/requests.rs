use serde::Deserialize;
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

/// Optional segment filters for [`VisitorsListQuery`]. Each filter narrows the
/// result set to visitors who match the given dimension value within the date
/// range. All filters resolve against `visitor` / `ip_geolocations` — by
/// design we never touch the events hypertable here so filtering stays fast
/// regardless of event volume.
#[derive(Debug, Deserialize, Clone, Default, ToSchema)]
pub struct VisitorSegmentFilters {
    /// Geolocation country (matches `ip_geolocations.country`)
    pub filter_country: Option<String>,
    /// Geolocation region (matches `ip_geolocations.region`)
    pub filter_region: Option<String>,
    /// Geolocation city (matches `ip_geolocations.city`)
    pub filter_city: Option<String>,
    /// First-touch marketing channel (matches `visitor.first_channel`)
    pub filter_channel: Option<String>,
    /// First-touch referrer hostname (matches `visitor.first_referrer_hostname`)
    pub filter_referrer: Option<String>,
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
    /// Filter to only include visitors with recorded activity (events/sessions).
    /// When true, excludes "ghost" visitors that have no events.
    pub has_activity_only: Option<bool>,

    // Segment filters — drill into "visitors who match this dimension value".
    // Flattened so each filter remains a top-level query string param.
    #[serde(flatten)]
    pub segment: VisitorSegmentFilters,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct VisitorSessionsQuery {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub limit: Option<i32>,
}

#[derive(Deserialize, Clone, ToSchema)]
pub struct VisitorJourneyQuery {
    pub project_id: i32,
    pub limit_sessions: Option<i32>,
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
    pub visitor_id: Option<i32>,
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

/// Query parameters for page path visitors
#[derive(Deserialize, Clone, ToSchema)]
pub struct PagePathVisitorsQuery {
    /// The specific page path to get visitors for
    pub page_path: String,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub start_date: DateTime,
    pub end_date: DateTime,
    /// Page number (1-based, default: 1)
    pub page: Option<u64>,
    /// Items per page (default: 50, max: 100)
    pub per_page: Option<u64>,
}

/// Query parameters for page path detail analytics
#[derive(Deserialize, Clone, ToSchema)]
pub struct PagePathDetailQuery {
    /// The specific page path to get details for (URL-encoded)
    pub page_path: String,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub start_date: DateTime,
    pub end_date: DateTime,
    /// Bucket interval for time series: 'hour', 'day', 'week', 'month' (default: auto)
    pub bucket_interval: Option<String>,
}

/// Query parameters for event detail analytics
#[derive(Deserialize, Clone, ToSchema)]
pub struct EventDetailQuery {
    /// The specific event name to get details for
    pub event_name: String,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub start_date: DateTime,
    pub end_date: DateTime,
    /// Bucket interval for time series: 'hour', 'day', 'week', 'month' (default: auto)
    pub bucket_interval: Option<String>,
}

/// Query parameters for event visitors list
#[derive(Deserialize, Clone, ToSchema)]
pub struct EventVisitorsQuery {
    /// The specific event name to list visitors for
    pub event_name: String,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub start_date: DateTime,
    pub end_date: DateTime,
    /// Page number (1-based, default: 1)
    pub page: Option<u64>,
    /// Items per page (default: 20, max: 100)
    pub per_page: Option<u64>,
}

/// Query parameters for the raw event entries list
#[derive(Deserialize, Clone, ToSchema)]
pub struct EventEntriesQuery {
    /// The specific event name to list occurrences for
    pub event_name: String,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub start_date: DateTime,
    pub end_date: DateTime,
    /// Page number (1-based, default: 1)
    pub page: Option<u64>,
    /// Items per page (default: 20, max: 100)
    pub per_page: Option<u64>,
}

/// Query parameters for page flow analytics
#[derive(Deserialize, Clone, ToSchema)]
pub struct PageFlowQuery {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub start_date: DateTime,
    pub end_date: DateTime,
    /// Maximum number of entry/exit pages to return (default: 20)
    pub limit: Option<i32>,
    /// Maximum number of transitions to return (default: 50)
    pub transitions_limit: Option<i32>,
    /// Minimum views for drop-off analysis (default: 5)
    pub min_views_for_dropoff: Option<i32>,
}

/// Query parameters for the visitor-facets endpoint. Mirrors the shape of
/// `VisitorsListQuery` so the same segment filters apply — facet counts are
/// always computed against the *currently filtered* visitor pool, minus the
/// dimension being aggregated.
#[derive(Deserialize, Clone, ToSchema)]
pub struct VisitorFacetsQuery {
    pub start_date: DateTime,
    pub end_date: DateTime,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub include_crawlers: Option<bool>,
    pub has_activity_only: Option<bool>,
    /// Maximum number of values returned per dimension (default: 50, max: 200).
    pub per_facet_limit: Option<i32>,

    #[serde(flatten)]
    pub segment: VisitorSegmentFilters,
}
