//! Shared types for the IndexNow plugin.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ============================================================================
// Settings
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PluginSettings {
    /// IndexNow API key (8-128 hex chars). Must be set before submissions work.
    pub api_key: Option<String>,
    /// Search engine endpoint to submit to (default: api.indexnow.org)
    pub search_engine: String,
    /// Whether to auto-submit pages on deployment.succeeded events
    pub auto_submit: bool,
    /// Maximum number of pages to discover per site crawl
    pub max_pages: usize,
    /// Hours after which a previously-submitted page is considered stale
    pub resubmit_after_hours: u64,
    /// User-Agent string for HTTP requests
    pub user_agent: String,
}

impl PluginSettings {
    pub const DEFAULT_SEARCH_ENGINE: &'static str = "api.indexnow.org";
    pub const DEFAULT_AUTO_SUBMIT: bool = true;
    pub const DEFAULT_MAX_PAGES: usize = 100;
    pub const DEFAULT_RESUBMIT_AFTER_HOURS: u64 = 48;
    pub const DEFAULT_USER_AGENT: &'static str = "TempsBot/1.0 (+https://temps.sh/bot)";
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSettings {
    pub api_key: Option<String>,
    pub search_engine: Option<String>,
    pub auto_submit: Option<bool>,
    pub max_pages: Option<usize>,
    pub resubmit_after_hours: Option<u64>,
    pub user_agent: Option<String>,
}

// ============================================================================
// Submission tracking
// ============================================================================

/// Data needed to record a submission in the store.
#[derive(Debug, Clone)]
pub struct SubmissionRecord {
    pub url: String,
    pub host: String,
    pub last_modified_at: Option<String>,
    pub etag: Option<String>,
    pub content_hash: Option<String>,
    pub last_status_code: Option<i32>,
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
    pub last_submitted_at: String,
    pub last_modified_at: Option<String>,
    pub submission_count: i32,
    pub deployment_id: Option<i32>,
    pub project_id: Option<i32>,
}

/// A page that might need resubmission.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PageSuggestion {
    pub url: String,
    pub host: String,
    /// Why we think this page needs resubmission
    pub reason: SuggestionReason,
    /// When the page was last submitted
    pub last_submitted_at: Option<String>,
    /// When the page was last modified (from HTTP header or HTML meta)
    pub last_modified_at: Option<String>,
    /// Current last-modified from the live page (if we checked)
    pub current_last_modified: Option<String>,
    /// Whether the content hash changed since last submission
    pub content_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionReason {
    /// Never submitted before
    NeverSubmitted,
    /// Last submission was older than the configured threshold
    StaleSubmission,
    /// The page's Last-Modified header is newer than our last submission
    ContentModified,
    /// The page's content hash differs from our last submission
    ContentHashChanged,
    /// New page discovered in this crawl
    NewPage,
}

/// Result of an IndexNow submission batch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionResult {
    /// Number of URLs submitted
    pub submitted_count: usize,
    /// Number of URLs that were skipped (already fresh)
    pub skipped_count: usize,
    /// Number of URLs that failed
    pub failed_count: usize,
    /// HTTP status code from IndexNow API
    pub api_status: Option<u16>,
    /// Error message if the submission failed
    pub error: Option<String>,
    /// The URLs that were submitted
    pub submitted_urls: Vec<String>,
}

/// Detailed info about a crawled page.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrawledPage {
    pub url: String,
    pub status_code: u16,
    pub last_modified: Option<String>,
    pub etag: Option<String>,
    pub content_hash: String,
    /// Links found on this page
    pub links: Vec<String>,
}

// ============================================================================
// API request types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRequest {
    /// URLs to submit. If empty, auto-discover from the site.
    pub urls: Option<Vec<String>>,
    /// Base URL to crawl for page discovery (required if urls is empty)
    pub site_url: Option<String>,
    /// Optional project_id context
    pub project_id: Option<i32>,
    /// Optional environment_id context
    pub environment_id: Option<i32>,
    /// Optional deployment_id context
    pub deployment_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionsRequest {
    /// Base URL of the site to check
    pub site_url: String,
    /// Optional project_id filter
    pub project_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionsResponse {
    pub suggestions: Vec<PageSuggestion>,
    pub total_pages_checked: usize,
    pub pages_needing_submission: usize,
}
