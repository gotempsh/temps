//! Data structures for weekly digest

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

/// Sections that can be included in the weekly digest
/// Note: `#[serde(default)]` allows backward compatibility when deserializing
/// old data that may have `security` and `resources` fields instead of `projects`
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct DigestSections {
    pub performance: bool,
    pub deployments: bool,
    pub errors: bool,
    pub funnels: bool,
    #[serde(default = "default_projects_enabled")]
    pub projects: bool,
}

fn default_projects_enabled() -> bool {
    true
}

impl Default for DigestSections {
    fn default() -> Self {
        Self {
            performance: true,
            deployments: true,
            errors: true,
            funnels: true,
            projects: true,
        }
    }
}

/// Complete weekly digest data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeeklyDigestData {
    pub week_start: DateTime<Utc>,
    pub week_end: DateTime<Utc>,
    pub project_name: Option<String>,
    pub executive_summary: ExecutiveSummary,
    pub performance: Option<PerformanceData>,
    pub deployments: Option<DeploymentData>,
    pub errors: Option<ErrorData>,
    pub funnels: Option<FunnelData>,
    pub projects: Vec<ProjectStats>,
}

/// Executive summary with key metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutiveSummary {
    pub total_visitors: i64,
    pub visitor_change_percent: f64,
    pub total_deployments: i64,
    pub failed_deployments: i64,
    pub new_errors: i64,
    pub uptime_percent: f64,
}

/// Performance and analytics data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceData {
    pub total_visitors: i64,
    pub unique_sessions: i64,
    pub page_views: i64,
    pub average_session_duration: f64, // in minutes
    pub bounce_rate: f64,
    pub top_pages: Vec<TopPage>,
    pub geographic_distribution: Vec<GeographicData>,
    pub visitor_trend: Vec<TrendPoint>,
    pub week_over_week_change: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopPage {
    pub path: String,
    pub views: i64,
    pub unique_visitors: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeographicData {
    pub country: String,
    pub visitors: i64,
    pub percentage: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendPoint {
    pub date: DateTime<Utc>,
    pub value: i64,
}

/// Deployment and infrastructure data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentData {
    pub total_deployments: i64,
    pub successful_deployments: i64,
    pub failed_deployments: i64,
    pub success_rate: f64,
    pub average_duration: f64, // in minutes
    pub preview_environments_created: i64,
    pub preview_environments_destroyed: i64,
    pub most_active_projects: Vec<ActiveProject>,
    pub deployment_trend: Vec<TrendPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveProject {
    pub project_name: String,
    pub deployment_count: i64,
    pub success_rate: f64,
}

/// Error and reliability data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorData {
    pub total_errors: i64,
    pub error_rate: f64,
    pub new_error_types: i64,
    pub most_common_errors: Vec<CommonError>,
    pub affected_users: i64,
    pub error_trend: Vec<TrendPoint>,
    pub uptime_percentage: f64,
    pub failed_health_checks: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommonError {
    pub error_type: String,
    pub count: i64,
    pub first_occurrence: DateTime<Utc>,
    pub last_occurrence: DateTime<Utc>,
    pub affected_sessions: i64,
}

/// Funnel and conversion data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunnelData {
    pub total_funnels: i64,
    pub funnel_stats: Vec<FunnelStat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunnelStat {
    pub funnel_name: String,
    pub completion_rate: f64,
    pub drop_off_rate: f64,
    pub week_over_week_change: f64,
    pub total_entries: i64,
    pub total_completions: i64,
}

/// Security and access data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityData {
    pub new_user_signups: i64,
    pub git_provider_connections: i64,
    pub api_key_usage: i64,
    pub suspicious_activities: Vec<SuspiciousActivity>,
    pub audit_log_summary: HashMap<String, i64>, // operation_type -> count
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuspiciousActivity {
    pub activity_type: String,
    pub count: i64,
    pub description: String,
}

/// Individual project statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectStats {
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    pub visitors: i64,
    pub page_views: i64,
    pub unique_sessions: i64,
    pub deployments: i64,
    pub week_over_week_change: f64,
}

impl WeeklyDigestData {
    /// Create a new weekly digest with default/empty values
    pub fn new(week_start: DateTime<Utc>, week_end: DateTime<Utc>) -> Self {
        Self {
            week_start,
            week_end,
            project_name: None,
            executive_summary: ExecutiveSummary {
                total_visitors: 0,
                visitor_change_percent: 0.0,
                total_deployments: 0,
                failed_deployments: 0,
                new_errors: 0,
                uptime_percent: 100.0,
            },
            performance: None,
            deployments: None,
            errors: None,
            funnels: None,
            projects: Vec::new(),
        }
    }

    /// Check if any data is available for the digest
    pub fn has_data(&self) -> bool {
        self.performance.is_some()
            || self.deployments.is_some()
            || self.errors.is_some()
            || self.funnels.is_some()
            || !self.projects.is_empty()
    }
}
