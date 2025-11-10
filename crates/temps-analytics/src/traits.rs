use async_trait::async_trait;
use serde_json;
use temps_core::UtcDateTime;

use crate::types::responses::{
    EnrichVisitorResponse, EventCount, SessionDetails, SessionEventsResponse, SessionLogsResponse,
    VisitorDetails, VisitorSessionsResponse, VisitorsResponse,
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

    /// Get visitors list
    async fn get_visitors(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        include_crawlers: Option<bool>,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> Result<VisitorsResponse, AnalyticsError>;

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

    /// Get comprehensive visitor statistics
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
}
