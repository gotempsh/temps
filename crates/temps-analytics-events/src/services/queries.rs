//! Query value-types for the analytics read surface.
//!
//! Each `*Spec` struct captures the parameter space of one trait method on
//! [`crate::services::traits::AnalyticsEvents`]. Backends consume these
//! values and render them however they want — Timescale builds parameterized
//! SQL, ClickHouse builds typed query-builder calls, an in-memory mock just
//! filters a `Vec`. No backend ever sees the SQL of another.
//!
//! Validation lives in the constructor: `limit` clamps, `bucket_size`
//! defaults, `aggregation_level` parses. Once a `*Spec` is built, every
//! field is meaningful — backends never have to guess "what does `None`
//! mean here."
//!
//! All structs are `Clone + Debug` so they can be logged, fed into a parity
//! test harness, and replayed against multiple backends.
//!
//! Naming convention: `XxxSpec` for the backend-facing query specification
//! (this module). The HTTP request DTOs in `crate::types::requests` keep
//! their `XxxQuery` names — they're a different concept (JSON shape from
//! the wire) and can collide otherwise. Trait methods are named
//! `query_xxx(&self, q: XxxSpec)` so the dispatch is unambiguous.

use temps_core::UtcDateTime;

use crate::types::{AggregationLevel, PropertyBreakdownFilters, PropertyColumn};

/// Hard cap on `limit` parameters. Matches the project-wide pagination rule
/// (default 20, max 100) called out in CLAUDE.md.
const MAX_LIMIT: i32 = 100;

/// Helper applied at construction time.
fn clamp_limit(requested: Option<i32>, default: i32) -> i32 {
    requested.unwrap_or(default).clamp(1, MAX_LIMIT)
}

// ---------------------------------------------------------------------------
// Shared sub-types
// ---------------------------------------------------------------------------

/// Inclusive time range applied to every windowed query.
///
/// Constructed once in the handler from request params; backends never
/// re-parse strings. `start <= end` is enforced at construction.
#[derive(Debug, Clone, Copy)]
pub struct TimeRange {
    pub start: UtcDateTime,
    pub end: UtcDateTime,
}

impl TimeRange {
    /// Returns `None` if `start > end`. Handlers should produce a
    /// `Validation` error in that case rather than passing through.
    pub fn new(start: UtcDateTime, end: UtcDateTime) -> Option<Self> {
        if start > end {
            None
        } else {
            Some(Self { start, end })
        }
    }
}

/// Project + environment + deployment scope. Most queries are project-scoped;
/// `environment_id` and `deployment_id` further narrow.
#[derive(Debug, Clone, Copy)]
pub struct AnalyticsScope {
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
}

impl AnalyticsScope {
    pub fn project(project_id: i32) -> Self {
        Self {
            project_id,
            environment_id: None,
            deployment_id: None,
        }
    }

    pub fn with_environment(mut self, environment_id: Option<i32>) -> Self {
        self.environment_id = environment_id;
        self
    }

    pub fn with_deployment(mut self, deployment_id: Option<i32>) -> Self {
        self.deployment_id = deployment_id;
        self
    }
}

// ---------------------------------------------------------------------------
// Per-method query types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct EventsCountSpec {
    pub range: TimeRange,
    pub scope: AnalyticsScope,
    pub aggregation_level: AggregationLevel,
    pub limit: i32,
    pub custom_events_only: bool,
}

impl EventsCountSpec {
    pub fn new(
        range: TimeRange,
        scope: AnalyticsScope,
        aggregation_level: AggregationLevel,
        limit: Option<i32>,
        custom_events_only: Option<bool>,
    ) -> Self {
        Self {
            range,
            scope,
            aggregation_level,
            limit: clamp_limit(limit, 20),
            // Default true — exclude system events like page_view/heartbeat.
            // Mirrors prior behavior in get_events_count.
            custom_events_only: custom_events_only.unwrap_or(true),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionEventsSpec {
    pub session_id: String,
    pub scope: AnalyticsScope,
}

#[derive(Debug, Clone, Copy)]
pub struct HasEventsSpec {
    pub scope: AnalyticsScope,
}

#[derive(Debug, Clone, Copy)]
pub struct EventTypeBreakdownSpec {
    pub range: TimeRange,
    pub scope: AnalyticsScope,
    pub aggregation_level: AggregationLevel,
}

#[derive(Debug, Clone)]
pub struct EventsTimelineSpec {
    pub range: TimeRange,
    pub scope: AnalyticsScope,
    pub aggregation_level: AggregationLevel,
    pub event_name: Option<String>,
    /// Time bucket size (e.g. `"5 minutes"`, `"1 hour"`). `None` lets the
    /// backend auto-pick based on range width.
    pub bucket_size: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PropertyBreakdownSpec {
    pub range: TimeRange,
    pub scope: AnalyticsScope,
    pub event_name: Option<String>,
    pub group_by_column: PropertyColumn,
    /// String for now to match the existing API surface; consider lifting to
    /// a typed enum in a follow-up.
    pub aggregation_level: String,
    pub limit: i32,
    pub filters: Option<PropertyBreakdownFilters>,
}

impl PropertyBreakdownSpec {
    pub fn new(
        range: TimeRange,
        scope: AnalyticsScope,
        event_name: Option<String>,
        group_by_column: PropertyColumn,
        aggregation_level: impl Into<String>,
        limit: Option<i32>,
        filters: Option<PropertyBreakdownFilters>,
    ) -> Self {
        Self {
            range,
            scope,
            event_name,
            group_by_column,
            aggregation_level: aggregation_level.into(),
            limit: clamp_limit(limit, 20),
            filters,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PropertyTimelineSpec {
    pub range: TimeRange,
    pub scope: AnalyticsScope,
    pub event_name: Option<String>,
    pub group_by_column: PropertyColumn,
    pub aggregation_level: String,
    pub bucket_size: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct ActiveVisitorsSpec {
    pub scope: AnalyticsScope,
}

#[derive(Debug, Clone, Copy)]
pub struct HourlyVisitsSpec {
    pub range: TimeRange,
    pub scope: AnalyticsScope,
    pub aggregation_level: AggregationLevel,
}

#[derive(Debug, Clone)]
pub struct UniqueCountsSpec {
    pub range: TimeRange,
    pub scope: AnalyticsScope,
    /// What to count: `"sessions"`, `"visitors"`, `"returning_visitors"`, or
    /// `"page_views"`. The backend validates and returns a Validation error for
    /// unknown values.
    pub metric: String,
}

#[derive(Debug, Clone)]
pub struct DashboardProjectsSpec {
    /// Project IDs to summarize. Empty input is allowed and returns an
    /// empty response without hitting the backend.
    pub project_ids: Vec<i32>,
    pub range: TimeRange,
}

#[derive(Debug, Clone)]
pub struct AggregatedBucketsSpec {
    pub range: TimeRange,
    pub scope: AnalyticsScope,
    pub aggregation_level: AggregationLevel,
    /// Bucket size string (e.g. `"5 minutes"`). Required — the unified
    /// observe page always specifies one.
    pub bucket_size: String,
}
