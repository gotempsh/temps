// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request, response and error types of conversion goals (ADR 071 D2).
//!
//! These shapes are deliberately flat: a goal is `kind` plus the fields that
//! kind uses, so other features (ADR 072 experiments) can store a goal id and
//! read [`AnalyticsGoal`] without learning a tagged union.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::{IntoParams, ToSchema};

use crate::services::SmartFilter;

/// Most goals one project may define. Every goal is one branch of the
/// conversion query, so this bounds its cost.
pub const MAX_GOALS_PER_PROJECT: u64 = 50;
/// Most filters one goal may carry.
pub const MAX_GOAL_FILTERS: usize = 10;
pub const MAX_GOAL_NAME_LEN: usize = 100;
pub const MAX_GOAL_EVENT_NAME_LEN: usize = 255;
pub const MAX_GOAL_PAGE_PATH_LEN: usize = 512;
pub const MAX_GOAL_PROPERTY_LEN: usize = 64;
/// Rows a goal breakdown returns at most.
pub const MAX_GOAL_BREAKDOWN_LIMIT: u64 = 100;

/// What a goal matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsGoalKind {
    /// An event, by name (`COALESCE(event_name, event_type)`), plus filters.
    Event,
    /// A page view whose path matches `page_path`.
    Page,
}

impl AnalyticsGoalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AnalyticsGoalKind::Event => "event",
            AnalyticsGoalKind::Page => "page",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "event" => Some(AnalyticsGoalKind::Event),
            "page" => Some(AnalyticsGoalKind::Page),
            _ => None,
        }
    }
}

/// Body of `POST` and `PUT /projects/{project_id}/analytics/goals`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AnalyticsGoalInput {
    /// Shown in the Conversions report, e.g. "Signup". Unique per project,
    /// ignoring case.
    #[schema(example = "Signup")]
    pub name: String,
    pub kind: AnalyticsGoalKind,
    /// Required for `event` goals, refused for `page` goals.
    #[schema(example = "signup")]
    pub event_name: Option<String>,
    /// Conditions every matching event must meet, in the vocabulary funnel
    /// steps use, e.g. `[{"type":"custom_data","value":{"path":"plan","value":"pro"}}]`.
    #[serde(default)]
    pub event_filter: Vec<SmartFilter>,
    /// Required for `page` goals, refused for `event` goals. Starts with `/`;
    /// `*` matches any run of characters, e.g. `/thank-you` or `/docs/*`.
    #[schema(example = "/thank-you")]
    pub page_path: Option<String>,
    /// Event property holding the value of one conversion, e.g. `amount`.
    /// Event goals only; requires `revenue_currency`.
    #[schema(example = "amount")]
    pub revenue_property: Option<String>,
    /// ISO 4217 currency code of `revenue_property`, e.g. `EUR`.
    #[schema(example = "EUR")]
    pub revenue_currency: Option<String>,
}

/// A conversion goal.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AnalyticsGoal {
    pub id: i32,
    pub project_id: i32,
    pub name: String,
    pub kind: AnalyticsGoalKind,
    pub event_name: Option<String>,
    pub event_filter: Vec<SmartFilter>,
    pub page_path: Option<String>,
    pub revenue_property: Option<String>,
    pub revenue_currency: Option<String>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: DateTime<Utc>,
}

/// A page of goals, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AnalyticsGoalListResponse {
    pub items: Vec<AnalyticsGoal>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
pub struct AnalyticsGoalListQuery {
    /// 1-based page, default 1.
    pub page: Option<u64>,
    /// Default 20, at most 100.
    pub page_size: Option<u64>,
}

/// Period and environment the conversion figures cover.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct AnalyticsGoalStatsQuery {
    /// Inclusive start (ISO 8601).
    #[param(value_type = String, format = DateTime, example = "2026-11-01T00:00:00Z")]
    pub start_date: DateTime<Utc>,
    /// Exclusive end (ISO 8601).
    #[param(value_type = String, format = DateTime, example = "2026-11-08T00:00:00Z")]
    pub end_date: DateTime<Utc>,
    pub environment_id: Option<i32>,
}

/// Revenue a goal's conversions carried.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct AnalyticsGoalRevenue {
    pub currency: String,
    /// Sum of the revenue property over conversions that carried a number.
    pub total: f64,
    /// `total` divided by the conversions that carried a number.
    pub average: f64,
}

/// Conversion figures of one goal in a period.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AnalyticsGoalStats {
    pub goal_id: i32,
    pub name: String,
    pub kind: AnalyticsGoalKind,
    /// Distinct visitors who converted.
    pub unique_conversions: i64,
    /// Matching events, a visitor converting twice counting twice.
    pub total_conversions: i64,
    /// `unique_conversions` as a percentage of the period's visitors.
    pub conversion_rate: f64,
    /// Present when the goal names a revenue property.
    pub revenue: Option<AnalyticsGoalRevenue>,
}

/// Response of `GET /projects/{project_id}/analytics/goals/stats`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AnalyticsGoalStatsResponse {
    /// Distinct non-crawler visitors with a page view in the period: the
    /// denominator of every conversion rate, as on the overview.
    pub visitors: i64,
    /// Every goal of the project, in creation order, including the ones
    /// without conversions in the period.
    pub goals: Vec<AnalyticsGoalStats>,
}

/// What a goal breakdown groups converting visitors by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsGoalBreakdownDimension {
    /// The visitor's first-touch channel.
    Channel,
    /// The visitor's first-touch referrer host.
    Referrer,
    /// The visitor's first-touch `utm_source`.
    UtmSource,
    /// The visitor's first-touch `utm_medium`.
    UtmMedium,
    /// The visitor's first-touch `utm_campaign`.
    UtmCampaign,
    /// Page the conversion happened on.
    Page,
    /// Country of the converting request.
    Country,
    DeviceType,
    Browser,
    OperatingSystem,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct AnalyticsGoalBreakdownQuery {
    pub dimension: AnalyticsGoalBreakdownDimension,
    /// Inclusive start (ISO 8601).
    #[param(value_type = String, format = DateTime, example = "2026-11-01T00:00:00Z")]
    pub start_date: DateTime<Utc>,
    /// Exclusive end (ISO 8601).
    #[param(value_type = String, format = DateTime, example = "2026-11-08T00:00:00Z")]
    pub end_date: DateTime<Utc>,
    pub environment_id: Option<i32>,
    /// Rows to return, default 10, at most 100.
    pub limit: Option<u64>,
}

/// One value of a goal breakdown.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AnalyticsGoalBreakdownRow {
    /// `null` when the visitor or event has no value for the dimension.
    pub value: Option<String>,
    /// Distinct non-crawler visitors with that value in the period.
    pub visitors: i64,
    pub unique_conversions: i64,
    pub total_conversions: i64,
    /// `unique_conversions` as a percentage of `visitors`.
    pub conversion_rate: f64,
    /// Revenue total and average, when the goal names a revenue property.
    pub revenue: Option<AnalyticsGoalRevenue>,
}

/// Response of `GET /projects/{project_id}/analytics/goals/{goal_id}/breakdown`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AnalyticsGoalBreakdownResponse {
    pub goal_id: i32,
    pub dimension: AnalyticsGoalBreakdownDimension,
    /// Ordered by unique conversions, then total conversions.
    pub items: Vec<AnalyticsGoalBreakdownRow>,
}

#[derive(Debug, Error)]
pub enum AnalyticsGoalError {
    #[error("Conversion goal {goal_id} not found in project {project_id}")]
    NotFound { project_id: i32, goal_id: i32 },

    #[error("Invalid conversion goal for project {project_id}: {message}")]
    Validation { project_id: i32, message: String },

    #[error("Project {project_id} already has a conversion goal named '{name}'")]
    DuplicateName { project_id: i32, name: String },

    #[error("Project {project_id} already has {limit} conversion goals, the most it may define")]
    LimitReached { project_id: i32, limit: u64 },

    #[error("Database error during {operation} for project {project_id}: {source}")]
    Database {
        project_id: i32,
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Stored conversion goal {goal_id} is unreadable: {reason}")]
    Corrupt { goal_id: i32, reason: String },
}
