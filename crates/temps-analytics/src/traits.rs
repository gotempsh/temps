use async_trait::async_trait;
use serde_json;
use temps_core::UtcDateTime;

use crate::types::requests::VisitorSegmentFilters;
use crate::types::responses::{
    EnrichVisitorResponse, EventCount, PageFlowResponse, SessionDetails, SessionEventsResponse,
    SessionLogsResponse, VisitorDetails, VisitorFacets, VisitorJourneyResponse,
    VisitorSessionsResponse, VisitorsResponse,
};
use crate::types::{AnalyticsError, Page};

/// Trait defining analytics operations for tracking and analyzing user behavior
#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait Analytics: Send + Sync {
    /// Get top pages by view count
    async fn get_top_pages(
        &self,
        project_id: i32,
        limit: u64,
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
    ) -> Result<Vec<Page>, AnalyticsError>;

    /// Get visitors list, optionally narrowed to a segment (e.g. visitors who
    /// used Chrome, came from the US, or triggered a specific event).
    async fn get_visitors(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        include_crawlers: Option<bool>,
        limit: Option<i32>,
        offset: Option<i32>,
        has_activity_only: Option<bool>,
        segment: VisitorSegmentFilters,
    ) -> Result<VisitorsResponse, AnalyticsError>;

    /// Get the top values per filter dimension for the visitors page filter
    /// UI. Counts are distinct visitor counts within the current date range
    /// and segment; each dimension is computed independently of itself so a
    /// selected value never collapses its own dropdown to a single option.
    async fn get_visitor_facets(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        include_crawlers: Option<bool>,
        has_activity_only: Option<bool>,
        per_facet_limit: Option<i32>,
        segment: VisitorSegmentFilters,
    ) -> Result<VisitorFacets, AnalyticsError>;

    /// Get event counts
    async fn get_events_count(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        limit: Option<i32>,
        custom_events_only: Option<bool>,
        breakdown: Option<crate::types::requests::EventBreakdown>,
    ) -> Result<Vec<EventCount>, AnalyticsError>;

    /// Get visitor basic info from database
    async fn get_visitor_info(
        &self,
        visitor_id: i32,
    ) -> Result<Option<crate::types::responses::VisitorRecord>, AnalyticsError>;

    /// Get visitor statistics
    async fn get_visitor_statistics(
        &self,
        visitor_id: i32,
    ) -> Result<Option<crate::types::responses::VisitorStats>, AnalyticsError>;

    /// Get visitor details by ID
    async fn get_visitor_details_by_id(
        &self,
        visitor_id: i32,
    ) -> Result<Option<VisitorDetails>, AnalyticsError>;

    /// Get visitor sessions by ID
    async fn get_visitor_sessions_by_id(
        &self,
        visitor_id: i32,
        limit: Option<i32>,
    ) -> Result<Option<VisitorSessionsResponse>, AnalyticsError>;

    /// Get the complete visitor journey: all events across all sessions, grouped by session
    async fn get_visitor_journey(
        &self,
        visitor_id: i32,
        project_id: i32,
        limit_sessions: Option<i32>,
    ) -> Result<Option<VisitorJourneyResponse>, AnalyticsError>;

    /// Get session details
    async fn get_session_details(
        &self,
        session_id: i32,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<Option<SessionDetails>, AnalyticsError>;

    /// Get session events
    async fn get_session_events(
        &self,
        session_id: i32,
        project_id: i32,
        environment_id: Option<i32>,
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
        limit: Option<i32>,
        offset: Option<i32>,
        sort_order: Option<String>,
    ) -> Result<Option<SessionEventsResponse>, AnalyticsError>;

    /// Get session logs
    async fn get_session_logs(
        &self,
        session_id: i32,
        project_id: i32,
        environment_id: Option<i32>,
        visitor_id: Option<i32>,
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
        limit: Option<i32>,
        offset: Option<i32>,
        sort_order: Option<String>,
    ) -> Result<Option<SessionLogsResponse>, AnalyticsError>;

    /// Enrich visitor by ID
    async fn enrich_visitor_by_id(
        &self,
        visitor_id: i32,
        enrichment_data: serde_json::Value,
    ) -> Result<EnrichVisitorResponse, AnalyticsError>;

    /// Enrich visitor by GUID (visitor_id string, may be encrypted with enc_ prefix)
    async fn enrich_visitor_by_guid(
        &self,
        visitor_guid: &str,
        enrichment_data: serde_json::Value,
    ) -> Result<EnrichVisitorResponse, AnalyticsError>;

    /// Check if analytics events exist
    async fn has_analytics_events(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<crate::types::responses::HasAnalyticsEventsResponse, AnalyticsError>;

    /// Get page paths
    async fn get_page_paths(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
        limit: Option<i32>,
    ) -> Result<crate::types::responses::PagePathsResponse, AnalyticsError>;

    /// Get active visitors count
    async fn get_active_visitors_count(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        minutes: Option<i32>,
    ) -> Result<i64, AnalyticsError>;

    /// Get active visitors details
    async fn get_active_visitors_details(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        minutes: Option<i32>,
        limit: Option<i32>,
    ) -> Result<crate::types::responses::ActiveVisitorsResponse, AnalyticsError>;

    /// Get sparkline data for multiple page paths in a single query
    async fn get_page_paths_sparklines(
        &self,
        project_id: i32,
        page_paths: &[String],
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
    ) -> Result<crate::types::responses::PagePathsSparklineResponse, AnalyticsError>;

    /// Get page hourly sessions
    async fn get_page_hourly_sessions(
        &self,
        project_id: i32,
        page_path: &str,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
    ) -> Result<crate::types::responses::PageHourlySessionsResponse, AnalyticsError>;

    /// Get visitor with geolocation by numeric ID
    async fn get_visitor_with_geolocation_by_id(
        &self,
        id: i32,
    ) -> Result<Option<crate::types::responses::VisitorWithGeolocation>, AnalyticsError>;

    /// Get visitor with geolocation by GUID
    async fn get_visitor_with_geolocation_by_guid(
        &self,
        visitor_id: &str,
    ) -> Result<Option<crate::types::responses::VisitorWithGeolocation>, AnalyticsError>;

    /// Get live visitors from visitor table with recent activity
    async fn get_live_visitors(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        window_minutes: i32,
    ) -> Result<Vec<crate::types::responses::LiveVisitorInfo>, AnalyticsError>;

    /// Get general stats across all projects
    async fn get_general_stats(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
    ) -> Result<crate::types::responses::GeneralStatsResponse, AnalyticsError>;

    /// Get individual visitor sessions for a specific page path
    async fn get_page_path_visitors(
        &self,
        project_id: i32,
        page_path: &str,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
        page: u64,
        per_page: u64,
    ) -> Result<crate::types::responses::PagePathVisitorsResponse, AnalyticsError>;

    /// Get page flow analytics: entry pages, exit pages, drop-off points, and transitions
    async fn get_page_flow(
        &self,
        project_id: i32,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
        limit: Option<i32>,
        transitions_limit: Option<i32>,
        min_views_for_dropoff: Option<i32>,
    ) -> Result<PageFlowResponse, AnalyticsError>;

    /// Get recent activity events for the live feed
    async fn get_recent_activity(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        since_id: Option<i64>,
        limit: Option<i32>,
    ) -> Result<crate::types::responses::RecentActivityResponse, AnalyticsError>;

    /// Get detailed analytics for a specific page path
    /// Returns visitors, page views, activity over time, geographic distribution, and referrers
    async fn get_page_path_detail(
        &self,
        project_id: i32,
        page_path: &str,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
        bucket_interval: Option<&str>,
    ) -> Result<crate::types::responses::PagePathDetailResponse, AnalyticsError>;

    /// Get detailed analytics for a specific event name
    /// Returns total count, unique visitors, timeline, referrers, countries, browsers
    async fn get_event_detail(
        &self,
        project_id: i32,
        event_name: &str,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
        bucket_interval: Option<&str>,
    ) -> Result<crate::types::responses::EventDetailResponse, AnalyticsError>;

    /// Get paginated list of visitors who triggered a specific event
    async fn get_event_visitors(
        &self,
        project_id: i32,
        event_name: &str,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
        page: u64,
        per_page: u64,
    ) -> Result<crate::types::responses::EventVisitorsResponse, AnalyticsError>;

    /// Get a paginated list of raw event occurrences for a specific event name,
    /// including each occurrence's custom JSON properties
    async fn get_event_entries(
        &self,
        project_id: i32,
        event_name: &str,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        environment_id: Option<i32>,
        page: u64,
        per_page: u64,
    ) -> Result<crate::types::responses::EventEntriesResponse, AnalyticsError>;
}
