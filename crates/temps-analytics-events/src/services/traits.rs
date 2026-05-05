//! Read-side trait for the analytics events service.
//!
//! Mirrors the existing `temps_analytics::Analytics` trait pattern. Defines
//! every read query that handlers depend on so the storage backend
//! (TimescaleDB today, ClickHouse later) can be swapped without touching
//! handler code.
//!
//! Writes (`record_event`) are intentionally excluded — they go through the
//! concrete service, and in the hybrid model will fan out to ClickHouse via
//! an outbox rather than being a backend choice point.

use async_trait::async_trait;
use temps_core::UtcDateTime;

use crate::services::events_service::EventsError;
use crate::types::{
    AggregatedBucketsResponse, AggregationLevel, AnalyticsSessionEventsResponse,
    DashboardProjectsAnalyticsResponse, EventCount, EventTimeline, EventTypeBreakdown,
    PropertyBreakdownFilters, PropertyBreakdownResponse, PropertyColumn, PropertyTimelineResponse,
    UniqueCountsResponse,
};

/// Read-side analytics queries. Implementations must produce identical results
/// for the same inputs across backends — any divergence is a bug.
#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait AnalyticsEvents: Send + Sync {
    /// Top events by count, optionally aggregated by sessions/visitors.
    async fn get_events_count(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        limit: Option<i32>,
        custom_events_only: Option<bool>,
        aggregation_level: AggregationLevel,
    ) -> Result<Vec<EventCount>, EventsError>;

    /// All events for a single session, ordered by timestamp.
    async fn get_session_events(
        &self,
        session_id: String,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<Option<AnalyticsSessionEventsResponse>, EventsError>;

    /// Whether the project has any events at all (for empty-state UI).
    async fn has_analytics_events(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<bool, EventsError>;

    /// Breakdown by event_type with optional aggregation level.
    async fn get_event_type_breakdown(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        aggregation_level: AggregationLevel,
    ) -> Result<Vec<EventTypeBreakdown>, EventsError>;

    /// Time-bucketed event counts.
    async fn get_events_timeline(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        event_name: Option<String>,
        bucket_size: Option<String>,
        aggregation_level: AggregationLevel,
    ) -> Result<Vec<EventTimeline>, EventsError>;

    /// Group events by a property column with counts.
    async fn get_property_breakdown(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        event_name: Option<String>,
        group_by_column: PropertyColumn,
        aggregation_level: &str,
        limit: Option<i32>,
        filters: Option<PropertyBreakdownFilters>,
    ) -> Result<PropertyBreakdownResponse, EventsError>;

    /// Property breakdown over time (group + bucket).
    async fn get_property_timeline(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        event_name: Option<String>,
        group_by_column: PropertyColumn,
        aggregation_level: &str,
        bucket_size: Option<String>,
    ) -> Result<PropertyTimelineResponse, EventsError>;

    /// Active visitors in the last 5 minutes (live counter).
    async fn get_active_visitors_count(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
    ) -> Result<i64, EventsError>;

    /// Hourly bucket counts for a date range.
    async fn get_hourly_visits(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        aggregation_level: AggregationLevel,
    ) -> Result<Vec<EventTimeline>, EventsError>;

    /// Unique sessions/visitors counts.
    async fn get_unique_counts(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        metric: String,
    ) -> Result<UniqueCountsResponse, EventsError>;

    /// Dashboard summary across multiple projects in one query.
    async fn get_dashboard_projects_analytics(
        &self,
        project_ids: &[i32],
        start_date: UtcDateTime,
        end_date: UtcDateTime,
    ) -> Result<DashboardProjectsAnalyticsResponse, EventsError>;

    /// Aggregated buckets used by the unified observe page.
    async fn get_aggregated_buckets(
        &self,
        start_date: UtcDateTime,
        end_date: UtcDateTime,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
        aggregation_level: AggregationLevel,
        bucket_size: String,
    ) -> Result<AggregatedBucketsResponse, EventsError>;
}
