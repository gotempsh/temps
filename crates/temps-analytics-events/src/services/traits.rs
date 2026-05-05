//! Read-side trait for the analytics events service.
//!
//! Each method takes a single query value-type from
//! [`crate::services::queries`] and returns a typed response DTO. Backends
//! consume the value and render it however they want — Timescale builds
//! parameterized SQL, ClickHouse builds typed query-builder calls, an
//! in-memory mock filters a `Vec`. The trait deliberately does **not**
//! describe SQL.
//!
//! Writes (`record_event`) are intentionally excluded — they go through the
//! concrete service, and in the hybrid model will fan out to ClickHouse via
//! an outbox rather than being a backend choice point.

use async_trait::async_trait;

use crate::services::events_service::EventsError;
use crate::services::queries::{
    ActiveVisitorsSpec, AggregatedBucketsSpec, DashboardProjectsSpec, EventTypeBreakdownSpec,
    EventsCountSpec, EventsTimelineSpec, HasEventsSpec, HourlyVisitsSpec, PropertyBreakdownSpec,
    PropertyTimelineSpec, SessionEventsSpec, UniqueCountsSpec,
};
use crate::types::{
    AggregatedBucketsResponse, AnalyticsSessionEventsResponse, DashboardProjectsAnalyticsResponse,
    EventCount, EventTimeline, EventTypeBreakdown, PropertyBreakdownResponse,
    PropertyTimelineResponse, UniqueCountsResponse,
};

/// Read-side analytics queries. Implementations must produce identical
/// results for the same inputs across backends — any divergence is a bug.
///
/// Each method takes a `*Query` value-type. The trait does not describe how
/// the backend renders the query; that's the implementation's choice.
#[async_trait]
pub trait AnalyticsEvents: Send + Sync {
    /// Top events by count, optionally aggregated by sessions/visitors.
    async fn query_events_count(&self, q: EventsCountSpec) -> Result<Vec<EventCount>, EventsError>;

    /// All events for a single session, ordered by timestamp.
    async fn query_session_events(
        &self,
        q: SessionEventsSpec,
    ) -> Result<Option<AnalyticsSessionEventsResponse>, EventsError>;

    /// Whether the project has any events at all (for empty-state UI).
    async fn query_has_events(&self, q: HasEventsSpec) -> Result<bool, EventsError>;

    /// Breakdown by event_type with optional aggregation level.
    async fn query_event_type_breakdown(
        &self,
        q: EventTypeBreakdownSpec,
    ) -> Result<Vec<EventTypeBreakdown>, EventsError>;

    /// Time-bucketed event counts.
    async fn query_events_timeline(
        &self,
        q: EventsTimelineSpec,
    ) -> Result<Vec<EventTimeline>, EventsError>;

    /// Group events by a property column with counts.
    async fn query_property_breakdown(
        &self,
        q: PropertyBreakdownSpec,
    ) -> Result<PropertyBreakdownResponse, EventsError>;

    /// Property breakdown over time (group + bucket).
    async fn query_property_timeline(
        &self,
        q: PropertyTimelineSpec,
    ) -> Result<PropertyTimelineResponse, EventsError>;

    /// Active visitors in the last 5 minutes (live counter).
    async fn query_active_visitors(&self, q: ActiveVisitorsSpec) -> Result<i64, EventsError>;

    /// Hourly bucket counts for a date range.
    async fn query_hourly_visits(
        &self,
        q: HourlyVisitsSpec,
    ) -> Result<Vec<EventTimeline>, EventsError>;

    /// Unique sessions/visitors counts.
    async fn query_unique_counts(
        &self,
        q: UniqueCountsSpec,
    ) -> Result<UniqueCountsResponse, EventsError>;

    /// Dashboard summary across multiple projects in one query.
    async fn query_dashboard_projects(
        &self,
        q: DashboardProjectsSpec,
    ) -> Result<DashboardProjectsAnalyticsResponse, EventsError>;

    /// Aggregated buckets used by the unified observe page.
    async fn query_aggregated_buckets(
        &self,
        q: AggregatedBucketsSpec,
    ) -> Result<AggregatedBucketsResponse, EventsError>;
}
