// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, JsonSchema, PartialEq)]
pub struct ActivityCategory {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ActivitySettings {
    pub application_context: String,
    pub categories: Vec<ActivityCategory>,
    /// Only these event-property keys may be sent to the provider.
    pub property_keys: Vec<String>,
    pub daily_enabled: bool,
    /// Explicit permission to send the selected analytics fields to the configured AI provider.
    pub share_activity_with_ai: bool,
    #[serde(default)]
    pub environment_id: Option<i32>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub source_domain: Option<String>,
}

impl Default for ActivitySettings {
    fn default() -> Self {
        Self {
            application_context: String::new(),
            categories: vec![
                ActivityCategory { name: "Learning".into(), description: "Reading educational content without clear evidence of evaluation.".into() },
                ActivityCategory { name: "Evaluating".into(), description: "Exploring pricing, comparisons, compatibility or migration.".into() },
                ActivityCategory { name: "Implementing".into(), description: "Setting up or using the product; prefer explicit success events over pageviews.".into() },
                ActivityCategory { name: "Seeking help".into(), description: "Troubleshooting or encountering repeated errors.".into() },
            ],
            property_keys: Vec::new(),
            daily_enabled: false,
            share_activity_with_ai: false,
            environment_id: None,
            source_url: None,
            source_domain: None,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ActivityGoalsRequest {
    pub url: String,
    pub share_with_ai: bool,
    #[serde(default)]
    pub environment_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActivityGoal {
    pub title: String,
    pub goal: String,
    pub rationale: String,
    pub missing_signals: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ModelGoals {
    pub goals: Vec<ActivityGoal>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ActivityGoals {
    pub goals: Vec<ActivityGoal>,
    pub pages_read: Vec<String>,
    pub model: String,
}

/// An unsaved onboarding preview. Sharing is explicit and scheduling is never inferred.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ActivityPreviewRequest {
    pub goal: String,
    pub share_activity_with_ai: bool,
    #[serde(default)]
    pub property_keys: Vec<String>,
    #[serde(default)]
    pub environment_id: Option<i32>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub source_domain: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ActivityPreview {
    pub settings: ActivitySettings,
    pub report: ActivityReport,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SuggestedCategories {
    pub categories: Vec<ActivityCategory>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ActivityProperty {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ActivityEvidence {
    /// Local to this report, not a database event ID.
    pub reference: u32,
    #[schema(value_type = String, format = DateTime)]
    pub timestamp: DateTime<Utc>,
    pub path: String,
    pub title: Option<String>,
    pub event: String,
    pub properties: Vec<ActivityProperty>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VisitorActivityAssessment {
    pub visitor_id: i32,
    pub categories: Vec<String>,
    pub explanation: String,
    pub evidence: Vec<ActivityEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ActivityReport {
    #[serde(default)]
    pub environment_id: Option<i32>,
    #[schema(value_type = String, format = DateTime)]
    pub started_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub completed_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub window_start: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub window_end: DateTime<Utc>,
    pub settings_revision: i32,
    pub categories: Vec<ActivityCategory>,
    pub model: Option<String>,
    pub summary: String,
    pub sampled: bool,
    pub events_considered: usize,
    pub visitors: Vec<VisitorActivityAssessment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ActivityStatus {
    /// Whether tracked non-crawler visitor events exist in the previous 24 hours.
    pub has_recent_activity: bool,
    pub selected_environment_id: Option<i32>,
    pub configured: bool,
    pub setup_url: String,
    pub settings: ActivitySettings,
    pub settings_revision: i32,
    pub running: bool,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub report: Option<ActivityReport>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct ModelAssessment {
    pub visitor_ref: u32,
    pub categories: Vec<String>,
    pub explanation: String,
    pub evidence_references: Vec<u32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct ModelReport {
    pub summary: String,
    pub visitors: Vec<ModelAssessment>,
}

#[derive(Debug, thiserror::Error)]
pub enum ActivityError {
    #[error("Project {project_id} was not found")]
    NotFound { project_id: i32 },
    #[error("Activity settings for project {project_id}: {reason}")]
    Validation { project_id: i32, reason: String },
    #[error(
        "Activity analysis for project {project_id} is running or cooling down; try again later"
    )]
    Busy { project_id: i32 },
    #[error("Configure an AI Gateway provider before analyzing activity for project {project_id}")]
    Unavailable { project_id: i32 },
    #[error("Activity analysis database operation '{operation}' failed for project {project_id}: {source}")]
    Database {
        project_id: i32,
        operation: &'static str,
        source: sea_orm::DbErr,
    },
    #[error("Activity analysis for project {project_id} failed: {reason}")]
    Analysis { project_id: i32, reason: String },
}
