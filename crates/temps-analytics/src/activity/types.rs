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
    /// Short operator-facing label for the selected analysis goal.
    #[serde(default)]
    pub goal_title: Option<String>,
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
    #[serde(default = "default_activity_threshold")]
    pub min_sessions: u32,
    #[serde(default = "default_activity_threshold")]
    pub min_page_paths: u32,
}

impl Default for ActivitySettings {
    fn default() -> Self {
        Self {
            application_context: String::new(),
            goal_title: None,
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
            min_sessions: default_activity_threshold(),
            min_page_paths: default_activity_threshold(),
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
    #[serde(default = "default_activity_threshold")]
    pub min_sessions: u32,
    #[serde(default = "default_activity_threshold")]
    pub min_page_paths: u32,
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
    #[serde(default)]
    pub skipped_low_activity: usize,
    #[serde(default)]
    pub skipped_unchanged: usize,
    pub visitors: Vec<VisitorActivityAssessment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ActivityStatus {
    /// Whether tracked non-crawler visitor events exist in the previous 24 hours.
    pub has_recent_activity: bool,
    pub selected_environment_id: Option<i32>,
    pub configured: bool,
    pub ai_provider: Option<String>,
    pub ai_model: Option<String>,
    pub setup_url: String,
    pub settings: ActivitySettings,
    pub settings_revision: i32,
    pub running: bool,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub report: Option<ActivityReport>,
    pub recent_runs: Vec<ActivityRunSummary>,
    /// The AI-assisted action running for this project right now, if any:
    /// a website analysis or preview started from the console, or a report run.
    #[serde(default)]
    pub in_progress: Option<ActivityInProgress>,
    /// Seconds until each action can use the AI provider again.
    #[serde(default)]
    pub cooldowns: ActivityCooldowns,
}

/// An AI-assisted activity action. Each one is limited to one at a time per
/// project and cools down briefly after it spends provider tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActivityOperation {
    /// "Analyze website": read public pages and suggest goals.
    WebsiteAnalysis,
    /// An unsaved preview of the report for a goal.
    Preview,
    /// A saved report run, manual or daily.
    Report,
}

impl ActivityOperation {
    pub const fn label(self) -> &'static str {
        match self {
            Self::WebsiteAnalysis => "Website analysis",
            Self::Preview => "Activity preview",
            Self::Report => "Activity report",
        }
    }

    /// The longest the action may run before it is abandoned.
    pub const fn time_limit(self) -> std::time::Duration {
        match self {
            Self::WebsiteAnalysis => std::time::Duration::from_secs(90),
            Self::Preview | Self::Report => std::time::Duration::from_secs(120),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ActivityInProgress {
    pub operation: ActivityOperation,
    #[schema(value_type = String, format = DateTime)]
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ActivityCooldowns {
    pub website_analysis_secs: Option<u64>,
    pub preview_secs: Option<u64>,
    pub report_secs: Option<u64>,
}

/// Machine-readable reason in an activity error response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActivityErrorCode {
    NotFound,
    Validation,
    AnalysisRunning,
    CoolingDown,
    ServerBusy,
    Superseded,
    AiUnavailable,
    ProviderError,
    SiteUnreadable,
    InvalidModelOutput,
    Internal,
}

/// Why the AI provider refused or failed the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActivityProviderFailure {
    Authentication,
    ModelNotFound,
    ModelNotAllowed,
    RateLimited,
    BudgetExceeded,
    Timeout,
    MalformedResponse,
    Unreachable,
    Misconfigured,
    Rejected,
    ProviderError,
}

impl From<temps_ai::ProviderFailureKind> for ActivityProviderFailure {
    fn from(kind: temps_ai::ProviderFailureKind) -> Self {
        use temps_ai::ProviderFailureKind as Kind;
        match kind {
            Kind::Authentication => Self::Authentication,
            Kind::ModelNotFound => Self::ModelNotFound,
            Kind::ModelNotAllowed => Self::ModelNotAllowed,
            Kind::RateLimited => Self::RateLimited,
            Kind::BudgetExceeded => Self::BudgetExceeded,
            Kind::Timeout => Self::Timeout,
            Kind::MalformedResponse => Self::MalformedResponse,
            Kind::Unreachable => Self::Unreachable,
            Kind::Misconfigured => Self::Misconfigured,
            Kind::Rejected => Self::Rejected,
            Kind::ProviderError => Self::ProviderError,
        }
    }
}

/// RFC 7807 body of every activity error. Fields other than `title`,
/// `detail` and `code` appear only when they apply to that code.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ActivityErrorBody {
    pub title: String,
    pub detail: String,
    pub code: ActivityErrorCode,
    /// The action that is running or cooling down.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<ActivityOperation>,
    /// When the running action started.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub started_at: Option<DateTime<Utc>>,
    /// Also sent as the `Retry-After` header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_failure: Option<ActivityProviderFailure>,
    /// The model the AI Gateway routed the request to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The upstream HTTP status, when the provider answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_status: Option<u16>,
    /// Console path where the operator can fix the problem.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_url: Option<String>,
}

/// Where the operator configures AI providers and models.
pub const AI_GATEWAY_SETTINGS_PATH: &str = "/ai-gateway";

pub const fn default_activity_threshold() -> u32 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ActivityRunSummary {
    pub trigger: String,
    pub status: String,
    #[schema(value_type = String, format = DateTime)]
    pub started_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub completed_at: Option<DateTime<Utc>>,
    pub environment_id: Option<i32>,
    pub analyzed_visitors: usize,
    pub skipped_visitors: usize,
    pub skipped_low_activity: usize,
    pub skipped_unchanged: usize,
    pub model: Option<String>,
    pub error: Option<String>,
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
        "{} for project '{project_name}' ({project_id}) is already running; it started at {started_at}",
        operation.label()
    )]
    AnalysisRunning {
        project_id: i32,
        project_name: String,
        operation: ActivityOperation,
        started_at: DateTime<Utc>,
        /// Upper bound: the action is abandoned after its time limit.
        retry_after_secs: u64,
    },
    #[error(
        "{} for project '{project_name}' ({project_id}) used the AI provider moments ago; retry in {retry_after_secs}s",
        operation.label()
    )]
    CoolingDown {
        project_id: i32,
        project_name: String,
        operation: ActivityOperation,
        retry_after_secs: u64,
    },
    #[error(
        "{} for project {project_id} is waiting for another project's analysis to free this server's analysis capacity; retry in {retry_after_secs}s",
        operation.label()
    )]
    ServerBusy {
        project_id: i32,
        operation: ActivityOperation,
        retry_after_secs: u64,
    },
    #[error("The activity report run for project '{project_name}' ({project_id}) was replaced by a newer run or settings change before it finished")]
    Superseded {
        project_id: i32,
        project_name: String,
    },
    #[error("Configure an AI Gateway provider before analyzing activity for project {project_id}")]
    Unavailable { project_id: i32 },
    /// `reason` can quote the provider's reply: it is logged, never returned.
    #[error("AI provider failed activity analysis for project {project_id} with model '{model}' ({kind}): {reason}")]
    Provider {
        project_id: i32,
        model: String,
        kind: temps_ai::ProviderFailureKind,
        status: Option<u16>,
        retry_after_secs: Option<u64>,
        reason: String,
    },
    #[error("Could not read public site {url} for project {project_id}: {reason}")]
    SiteUnreadable {
        project_id: i32,
        url: String,
        reason: String,
    },
    /// `reason` can quote the model's output: it is logged, never returned.
    #[error("Model '{model}' returned unusable output for project {project_id}: {reason}")]
    InvalidModelOutput {
        project_id: i32,
        model: String,
        reason: String,
    },
    #[error("Activity analysis database operation '{operation}' failed for project {project_id}: {source}")]
    Database {
        project_id: i32,
        operation: &'static str,
        source: sea_orm::DbErr,
    },
    #[error("Activity analysis for project {project_id} failed: {reason}")]
    Analysis { project_id: i32, reason: String },
}

impl ActivityError {
    /// What the operator is told. Names the project, never its internal id,
    /// and never quotes provider replies, prompts or credentials.
    pub fn user_message(&self) -> String {
        match self {
            Self::NotFound { .. } => "This project no longer exists.".into(),
            Self::Validation { reason, .. } => reason.clone(),
            Self::AnalysisRunning {
                project_name,
                operation,
                started_at,
                ..
            } => format!(
                "{} for {project_name} is already running (started {} UTC). Its result will appear here when it finishes.",
                operation.label(),
                started_at.format("%H:%M:%S")
            ),
            Self::CoolingDown {
                project_name,
                operation,
                retry_after_secs,
                ..
            } => format!(
                "{} for {project_name} used the AI provider moments ago. Try again in {retry_after_secs}s.",
                operation.label()
            ),
            Self::ServerBusy {
                retry_after_secs, ..
            } => format!(
                "Another project's activity analysis is using this server's analysis capacity. Try again in {retry_after_secs}s."
            ),
            Self::Superseded { project_name, .. } => format!(
                "A newer run or settings change for {project_name} replaced this report run before it finished. Run the report again."
            ),
            Self::Unavailable { .. } => {
                "No AI provider is configured. Add one in AI Gateway settings to analyze activity.".into()
            }
            Self::Provider {
                model,
                kind,
                status,
                retry_after_secs,
                ..
            } => provider_message(model, *kind, *status, *retry_after_secs),
            Self::SiteUnreadable { url, reason, .. } => {
                format!("Temps could not read {url}: {reason}")
            }
            Self::InvalidModelOutput { model, .. } => format!(
                "Model '{model}' returned a response Temps could not use. Try again, or choose a model that supports structured JSON output in AI Gateway settings."
            ),
            Self::Database { .. } | Self::Analysis { .. } => {
                "The activity analysis failed unexpectedly. Check the server logs and try again.".into()
            }
        }
    }
}

fn provider_message(
    model: &str,
    kind: temps_ai::ProviderFailureKind,
    status: Option<u16>,
    retry_after_secs: Option<u64>,
) -> String {
    use temps_ai::ProviderFailureKind as Kind;
    let status = status
        .map(|code| format!(" (HTTP {code})"))
        .unwrap_or_default();
    match kind {
        Kind::Authentication => format!(
            "The AI provider rejected the credentials for model '{model}' — check the provider key in AI Gateway settings."
        ),
        Kind::ModelNotFound => format!(
            "The AI provider rejected the request: model '{model}' not found — check AI Gateway settings."
        ),
        Kind::ModelNotAllowed => format!(
            "Model '{model}' is not in the AI Gateway's allowed models — check AI Gateway settings."
        ),
        Kind::RateLimited => match retry_after_secs {
            Some(seconds) => format!(
                "The AI provider rate-limited the request for model '{model}'. Try again in {seconds}s."
            ),
            None => format!(
                "The AI provider rate-limited the request for model '{model}'. Try again in a minute."
            ),
        },
        Kind::BudgetExceeded => {
            "This project's AI Gateway budget is spent — raise it in AI Gateway limits or wait for it to reset.".into()
        }
        Kind::Timeout => format!(
            "The AI provider did not answer in time for model '{model}'. Try again, or choose a faster model in AI Gateway settings."
        ),
        Kind::MalformedResponse => format!(
            "The AI provider returned an unreadable response for model '{model}'. Try again, or check the provider in AI Gateway settings."
        ),
        Kind::Unreachable => format!(
            "Temps could not reach the AI provider for model '{model}' — check its base URL and network access in AI Gateway settings."
        ),
        Kind::Misconfigured => format!(
            "The AI Gateway cannot route model '{model}' to a usable provider key — check AI Gateway settings."
        ),
        Kind::Rejected => format!(
            "The AI provider rejected the request for model '{model}'{status} — check AI Gateway settings."
        ),
        Kind::ProviderError => format!(
            "The AI provider failed the request for model '{model}'{status}. Try again later."
        ),
    }
}
