// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;

use serde::Serialize;
use temps_core::UtcDateTime;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct EventCount {
    pub event_name: String,
    pub count: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HasEventsResponse {
    pub has_events: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionEvent {
    pub id: i32,
    pub event_name: Option<String>,
    pub event_type: Option<String>,
    pub event_data: Option<serde_json::Value>,
    pub timestamp: String,
    pub page_url: Option<String>,
    pub page_title: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyticsSessionEventsResponse {
    pub session_id: String,
    pub events: Vec<SessionEvent>,
    pub total_events: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EventTypeBreakdown {
    pub event_type: String,
    pub count: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EventTimeline {
    #[schema(value_type = String, format = DateTime)]
    pub date: UtcDateTime,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EventPropertyValue {
    pub value: String,
    pub count: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EventPropertiesResponse {
    pub property_name: String,
    pub values: Vec<EventPropertyValue>,
    pub total_events: i64,
}

#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ActiveVisitorCountResponse)]
pub struct ActiveVisitorsResponse {
    pub active_visitors: i64,
    pub window_minutes: i32,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PropertyBreakdownItem {
    pub value: String,
    pub count: i64,
    pub percentage: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PropertyBreakdownResponse {
    pub property: String,
    pub items: Vec<PropertyBreakdownItem>,
    pub total: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PropertyTimelineItem {
    pub timestamp: String,
    pub value: String,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PropertyTimelineResponse {
    pub property: String,
    pub bucket_size: String,
    pub items: Vec<PropertyTimelineItem>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UniqueCountsResponse {
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AggregatedBucketItem {
    pub timestamp: String,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AggregatedBucketsResponse {
    pub bucket_size: String,
    pub aggregation_level: String,
    pub items: Vec<AggregatedBucketItem>,
    pub total: i64,
}

/// Analytics data for a single project in the dashboard batch response
#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectDashboardAnalytics {
    pub project_id: i32,
    /// Unique visitor count in the current time range
    pub unique_visitors: i64,
    /// Unique visitor count in the previous period (same duration, shifted back)
    pub previous_unique_visitors: i64,
    /// Percentage change from previous period (positive = growth, negative = decline)
    /// Null when previous period had zero visitors (no baseline to compare)
    pub trend_percentage: Option<f64>,
    /// Hourly sparkline data points
    pub hourly_visits: Vec<EventTimeline>,
}

/// Batch response for dashboard project analytics
#[derive(Debug, Serialize, ToSchema)]
pub struct DashboardProjectsAnalyticsResponse {
    /// Map of project_id -> analytics data
    pub projects: HashMap<String, ProjectDashboardAnalytics>,
}
