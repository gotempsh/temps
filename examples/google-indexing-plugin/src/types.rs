//! Shared types for the Google Indexing API plugin.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ============================================================================
// Settings
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PluginSettings {
    /// Whether the service account key has been configured
    pub service_account_configured: bool,
    /// The service account email (read-only, extracted from key)
    pub service_account_email: Option<String>,
    /// Whether to auto-submit pages on deployment.succeeded events
    pub auto_submit: bool,
    /// Maximum number of URLs to submit per deployment (quota-aware)
    pub max_urls_per_deploy: usize,
    /// Daily quota limit (Google default is 200/day)
    pub daily_quota: usize,
    /// Number of URLs submitted today
    pub urls_submitted_today: usize,
}

impl PluginSettings {
    pub const DEFAULT_AUTO_SUBMIT: bool = true;
    pub const DEFAULT_MAX_URLS_PER_DEPLOY: usize = 50;
    pub const DEFAULT_DAILY_QUOTA: usize = 200;
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSettings {
    pub auto_submit: Option<bool>,
    pub max_urls_per_deploy: Option<usize>,
    pub daily_quota: Option<usize>,
}

// ============================================================================
// Service account key (Google JSON key file format)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAccountKey {
    #[serde(rename = "type")]
    pub key_type: String,
    pub project_id: String,
    pub private_key_id: String,
    pub private_key: String,
    pub client_email: String,
    pub client_id: String,
    pub auth_uri: String,
    pub token_uri: String,
}

// ============================================================================
// Submission tracking
// ============================================================================

/// Data needed to record a submission in the store.
#[derive(Debug, Clone)]
pub struct SubmissionRecord {
    pub url: String,
    pub host: String,
    pub notification_type: String,
    pub google_response_status: Option<i32>,
    pub notify_time: Option<String>,
    pub deployment_id: Option<i32>,
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
}

// ============================================================================
// API response types
// ============================================================================

/// Summary of a single submission record for API responses.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionResponse {
    pub url: String,
    pub host: String,
    pub notification_type: String,
    pub submitted_at: String,
    pub google_response_status: Option<i32>,
    pub notify_time: Option<String>,
    pub submission_count: i32,
    pub deployment_id: Option<i32>,
    pub project_id: Option<i32>,
}

/// Result of a Google Indexing API submission batch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionResult {
    /// Number of URLs successfully submitted
    pub submitted_count: usize,
    /// Number of URLs skipped (quota, already recent, etc.)
    pub skipped_count: usize,
    /// Number of URLs that failed
    pub failed_count: usize,
    /// Per-URL results
    pub results: Vec<UrlSubmissionResult>,
    /// Error message if the entire batch failed
    pub error: Option<String>,
    /// Remaining daily quota after this submission
    pub remaining_quota: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UrlSubmissionResult {
    pub url: String,
    pub success: bool,
    pub status_code: Option<u16>,
    pub error: Option<String>,
    pub notify_time: Option<String>,
}

/// Quota status information.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuotaStatus {
    pub daily_limit: usize,
    pub used_today: usize,
    pub remaining: usize,
    pub resets_at: String,
}

/// URL notification metadata from Google.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UrlStatus {
    pub url: String,
    pub latest_update: Option<NotificationInfo>,
    pub latest_remove: Option<NotificationInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotificationInfo {
    pub url: String,
    #[serde(rename = "type")]
    pub notification_type: String,
    pub notify_time: String,
}

// ============================================================================
// API request types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRequest {
    /// URLs to submit as URL_UPDATED
    pub urls: Vec<String>,
    /// Notification type: "URL_UPDATED" or "URL_DELETED" (default: URL_UPDATED)
    pub notification_type: Option<String>,
    /// Optional project_id context
    pub project_id: Option<i32>,
    /// Optional environment_id context
    pub environment_id: Option<i32>,
    /// Optional deployment_id context
    pub deployment_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CheckStatusRequest {
    /// URL to check notification status for
    pub url: String,
}
