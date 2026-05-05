//! Shared DTOs used by every analytics backend implementation.
//!
//! These types describe the *contract*, not the storage layout. Both the
//! Timescale and ClickHouse backends translate them into engine-specific SQL.
//!
//! Phase 1 keeps this module intentionally minimal — only the inputs that the
//! trait method signatures need. As query methods are pulled out of
//! `temps-analytics-events::services::events_service` into the trait, their
//! input shapes are added here.

use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;

/// Aggregation level for event count queries.
///
/// Mirrors `temps_analytics_events::types::AggregationLevel` to avoid a
/// reverse dependency from this crate back to the events crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregationLevel {
    Events,
    Sessions,
    Visitors,
}

/// Time range filter applied to every analytics query.
#[derive(Debug, Clone)]
pub struct AnalyticsRange {
    pub start: UtcDateTime,
    pub end: UtcDateTime,
}

/// Project + environment scope for a query. Every analytics read is scoped to
/// at least a project; environment is optional (means "all environments").
#[derive(Debug, Clone, Copy)]
pub struct AnalyticsScope {
    pub project_id: i32,
    pub environment_id: Option<i32>,
}
