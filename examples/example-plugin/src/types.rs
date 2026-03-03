//! Shared types for the SEO Analyzer plugin.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ============================================================================
// Settings
// ============================================================================

/// Plugin-level configuration persisted in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PluginSettings {
    /// Default number of pages to crawl when not specified per-analysis.
    pub default_max_pages: usize,
    /// User-Agent string sent with crawl requests.
    pub user_agent: String,
    /// HTTP request timeout per page (seconds).
    pub request_timeout_secs: u64,
    /// Delay between requests to the same host (milliseconds).
    /// Prevents overwhelming the target server.
    pub crawl_delay_ms: u64,
}

impl PluginSettings {
    pub const DEFAULT_MAX_PAGES: usize = 50;
    pub const DEFAULT_USER_AGENT: &'static str = "TempsBot/1.0 (+https://temps.sh/bot)";
    pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 15;
    pub const DEFAULT_CRAWL_DELAY_MS: u64 = 100;
}

impl Default for PluginSettings {
    fn default() -> Self {
        Self {
            default_max_pages: Self::DEFAULT_MAX_PAGES,
            user_agent: Self::DEFAULT_USER_AGENT.to_string(),
            request_timeout_secs: Self::DEFAULT_REQUEST_TIMEOUT_SECS,
            crawl_delay_ms: Self::DEFAULT_CRAWL_DELAY_MS,
        }
    }
}

/// Partial update for plugin settings — only `Some` fields are written.
#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
pub struct UpdateSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_max_pages: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crawl_delay_ms: Option<u64>,
}

// ============================================================================
// Reports
// ============================================================================

/// Summary of a report (returned in list view).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReportSummary {
    pub id: String,
    pub url: String,
    pub score: u32,
    pub pages_crawled: usize,
    pub critical_issues: usize,
    pub warning_issues: usize,
    pub info_issues: usize,
    pub status: ReportStatus,
    pub created_at: String,
    pub duration_ms: u64,
}

/// Full SEO report with per-page analysis.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SeoReport {
    pub id: String,
    pub url: String,
    pub score: u32,
    pub pages: Vec<PageAnalysis>,
    pub summary: ReportSummaryStats,
    pub status: ReportStatus,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReportSummaryStats {
    pub pages_crawled: usize,
    pub total_issues: usize,
    pub critical: usize,
    pub warnings: usize,
    pub info: usize,
    pub avg_page_score: u32,
    pub missing_titles: usize,
    pub missing_descriptions: usize,
    pub missing_h1: usize,
    pub images_without_alt: usize,
    pub missing_canonical: usize,
    pub missing_og_tags: usize,
}

// ============================================================================
// Pages
// ============================================================================

/// Analysis result for a single page.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PageAnalysis {
    pub url: String,
    pub status_code: u16,
    pub score: u32,
    pub title: Option<String>,
    pub meta_description: Option<String>,
    pub canonical: Option<String>,
    pub h1_count: usize,
    pub h2_count: usize,
    pub image_count: usize,
    pub images_without_alt: usize,
    pub word_count: usize,
    pub internal_links: usize,
    pub external_links: usize,
    pub has_og_title: bool,
    pub has_og_description: bool,
    pub has_og_image: bool,
    pub has_robots_meta: bool,
    pub has_viewport: bool,
    pub has_charset: bool,
    pub has_lang: bool,
    pub load_time_ms: u64,
    pub issues: Vec<SeoIssue>,
}

// ============================================================================
// Issues
// ============================================================================

/// An individual SEO issue found on a page.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SeoIssue {
    pub severity: IssueSeverity,
    pub code: String,
    pub message: String,
    pub recommendation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Critical,
    Warning,
    Info,
}

// ============================================================================
// Request / response types
// ============================================================================

/// Request body for starting an analysis.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AnalyzeRequest {
    /// URL of the site to crawl (must be http or https).
    pub url: String,
    /// Number of pages to crawl. Falls back to plugin settings default if omitted.
    pub max_pages: Option<usize>,
}

/// Response when an analysis is successfully started.
#[derive(Debug, Serialize, ToSchema)]
pub struct AnalyzeResponse {
    /// Unique report ID (UUID).
    pub id: String,
    /// Always `"running"` for a newly started analysis.
    pub status: String,
    /// Human-readable confirmation message.
    pub message: String,
}
