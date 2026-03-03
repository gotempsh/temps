//! HTTP handlers for the SEO Analyzer plugin.
//!
//! All business logic lives in [`SeoStore`] and [`crawl`] — handlers are thin
//! wrappers that validate input, call the store, and return JSON responses.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use include_dir::Dir;
use url::Url;

use crate::crawl::{self, CrawlConfig};
use crate::db::SeoStore;
use crate::types::*;

// ============================================================================
// State
// ============================================================================

#[derive(Clone)]
pub struct AppState {
    pub store: SeoStore,
    pub http_client: reqwest::Client,
}

// ============================================================================
// UI Handlers — serve the embedded React SPA
// ============================================================================

/// Redirect /ui -> /ui/ so relative asset paths work correctly.
pub async fn redirect_to_ui() -> Response {
    Response::builder()
        .status(StatusCode::MOVED_PERMANENTLY)
        .header(header::LOCATION, "ui/")
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Serve the React SPA index.html.
pub async fn serve_ui_index() -> Response {
    serve_embedded_file(crate::ui_dist(), "index.html")
}

/// Serve a static asset from the embedded dist/ directory.
/// Falls back to index.html for client-side routing.
pub async fn serve_ui_asset(Path(path): Path<String>) -> Response {
    let dist = crate::ui_dist();
    if dist.get_file(&path).is_some() {
        return serve_embedded_file(dist, &path);
    }
    // SPA fallback — serve index.html for unmatched paths
    serve_embedded_file(dist, "index.html")
}

/// Serve a file from the compile-time embedded UI_DIST directory.
fn serve_embedded_file(dist: &Dir<'static>, path: &str) -> Response {
    match dist.get_file(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path)
                .first_or_octet_stream()
                .to_string();

            let cache = if path == "index.html" {
                "no-cache" // Don't cache the HTML shell
            } else {
                "public, max-age=31536000, immutable" // Cache hashed assets forever
            };

            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                .header(header::CACHE_CONTROL, cache)
                .body(Body::from(file.contents()))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("404 Not Found"))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    }
}

// ============================================================================
// API Handlers
// ============================================================================

/// Start an SEO analysis for a URL. Crawls the site in the background.
#[utoipa::path(
    post,
    path = "/analyze",
    tag = "SEO Analysis",
    request_body = AnalyzeRequest,
    responses(
        (status = 202, description = "Analysis started", body = AnalyzeResponse),
        (status = 400, description = "Invalid URL or scheme"),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
pub async fn start_analysis(
    State(state): State<AppState>,
    Json(req): Json<AnalyzeRequest>,
) -> Result<(StatusCode, Json<AnalyzeResponse>), (StatusCode, Json<serde_json::Value>)> {
    let parsed = Url::parse(&req.url).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Invalid URL: {}", e) })),
        )
    })?;

    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "URL must use http or https scheme" })),
        ));
    }

    // Resolve max_pages: per-request override → plugin default
    let settings = state.store.get_settings().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to load settings: {}", e) })),
        )
    })?;
    let max_pages = req.max_pages.unwrap_or(settings.default_max_pages);

    let report_id = uuid::Uuid::new_v4().to_string();

    state
        .store
        .create_report(&report_id, &req.url)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to create report: {}", e) })),
            )
        })?;

    let bg_store = state.store.clone();
    let bg_client = state.http_client.clone();
    let bg_url = req.url.clone();
    let bg_id = report_id.clone();
    let crawl_delay = std::time::Duration::from_millis(settings.crawl_delay_ms);

    tokio::spawn(async move {
        crawl::run_analysis(
            &bg_store,
            &bg_client,
            &bg_id,
            &bg_url,
            CrawlConfig {
                max_pages,
                crawl_delay,
            },
        )
        .await;
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(AnalyzeResponse {
            id: report_id,
            status: "running".into(),
            message: format!("Analysis started for {} (max {} pages)", req.url, max_pages),
        }),
    ))
}

/// List all reports (summary view).
#[utoipa::path(
    get,
    path = "/reports",
    tag = "SEO Reports",
    responses(
        (status = 200, description = "List of reports", body = Vec<ReportSummary>),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
pub async fn list_reports(
    State(state): State<AppState>,
) -> Result<Json<Vec<ReportSummary>>, (StatusCode, Json<serde_json::Value>)> {
    let reports = state.store.list_reports().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to list reports: {}", e) })),
        )
    })?;
    Ok(Json(reports))
}

/// Get a full report with per-page details.
#[utoipa::path(
    get,
    path = "/reports/{id}",
    tag = "SEO Reports",
    params(
        ("id" = String, Path, description = "Report ID")
    ),
    responses(
        (status = 200, description = "Full report", body = SeoReport),
        (status = 404, description = "Report not found"),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
pub async fn get_report(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SeoReport>, StatusCode> {
    match state.store.get_report(&id).await {
        Ok(Some(report)) => Ok(Json(report)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            tracing::error!(report_id = %id, error = %e, "Failed to get report");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Delete a report.
#[utoipa::path(
    delete,
    path = "/reports/{id}",
    tag = "SEO Reports",
    params(
        ("id" = String, Path, description = "Report ID")
    ),
    responses(
        (status = 204, description = "Report deleted"),
        (status = 404, description = "Report not found"),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
pub async fn delete_report(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    match state.store.delete_report(&id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Ok(StatusCode::NOT_FOUND),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to delete report: {}", e) })),
        )),
    }
}

/// Get plugin settings.
#[utoipa::path(
    get,
    path = "/settings",
    tag = "Settings",
    responses(
        (status = 200, description = "Plugin settings", body = PluginSettings),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
pub async fn get_settings(
    State(state): State<AppState>,
) -> Result<Json<PluginSettings>, (StatusCode, Json<serde_json::Value>)> {
    let settings = state.store.get_settings().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to load settings: {}", e) })),
        )
    })?;
    Ok(Json(settings))
}

/// Update plugin settings (partial update).
#[utoipa::path(
    patch,
    path = "/settings",
    tag = "Settings",
    request_body = UpdateSettings,
    responses(
        (status = 200, description = "Updated settings", body = PluginSettings),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
pub async fn update_settings(
    State(state): State<AppState>,
    Json(update): Json<UpdateSettings>,
) -> Result<Json<PluginSettings>, (StatusCode, Json<serde_json::Value>)> {
    let settings = state.store.update_settings(&update).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to update settings: {}", e) })),
        )
    })?;
    Ok(Json(settings))
}

/// Generate an LLM-friendly plain-text prompt containing all issues from a report.
///
/// Returns `text/plain` so the frontend can copy it directly to the clipboard
/// for pasting into ChatGPT, Claude, etc.
#[utoipa::path(
    get,
    path = "/reports/{id}/prompt",
    tag = "SEO Reports",
    params(
        ("id" = String, Path, description = "Report ID")
    ),
    responses(
        (status = 200, description = "LLM-friendly prompt as plain text"),
        (status = 404, description = "Report not found"),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
pub async fn get_report_prompt(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let report = match state.store.get_report(&id).await {
        Ok(Some(r)) => r,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(e) => {
            tracing::error!(report_id = %id, error = %e, "Failed to get report for prompt");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let prompt = format_report_as_prompt(&report);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(prompt))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
}

/// Format a full SEO report as a structured prompt for feeding to an LLM.
fn format_report_as_prompt(report: &SeoReport) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(4096);

    // Header
    writeln!(out, "# SEO Audit Report").ok();
    writeln!(out).ok();
    writeln!(out, "Site: {}", report.url).ok();
    writeln!(out, "Date: {}", report.created_at).ok();
    writeln!(out, "Overall Score: {}/100", report.score).ok();
    writeln!(out, "Pages Crawled: {}", report.summary.pages_crawled).ok();
    if report.duration_ms > 0 {
        writeln!(
            out,
            "Crawl Duration: {:.1}s",
            report.duration_ms as f64 / 1000.0
        )
        .ok();
    }
    writeln!(out).ok();

    // Summary stats
    writeln!(out, "## Summary").ok();
    writeln!(out).ok();
    writeln!(out, "- Total Issues: {}", report.summary.total_issues).ok();
    writeln!(out, "- Critical: {}", report.summary.critical).ok();
    writeln!(out, "- Warnings: {}", report.summary.warnings).ok();
    writeln!(out, "- Info: {}", report.summary.info).ok();
    writeln!(
        out,
        "- Average Page Score: {}/100",
        report.summary.avg_page_score
    )
    .ok();
    writeln!(out).ok();
    if report.summary.missing_titles > 0 {
        writeln!(
            out,
            "- Pages missing title: {}",
            report.summary.missing_titles
        )
        .ok();
    }
    if report.summary.missing_descriptions > 0 {
        writeln!(
            out,
            "- Pages missing meta description: {}",
            report.summary.missing_descriptions
        )
        .ok();
    }
    if report.summary.missing_h1 > 0 {
        writeln!(out, "- Pages missing H1: {}", report.summary.missing_h1).ok();
    }
    if report.summary.images_without_alt > 0 {
        writeln!(
            out,
            "- Images without alt text: {}",
            report.summary.images_without_alt
        )
        .ok();
    }
    if report.summary.missing_canonical > 0 {
        writeln!(
            out,
            "- Pages missing canonical: {}",
            report.summary.missing_canonical
        )
        .ok();
    }
    if report.summary.missing_og_tags > 0 {
        writeln!(
            out,
            "- Pages with incomplete OG tags: {}",
            report.summary.missing_og_tags
        )
        .ok();
    }
    writeln!(out).ok();

    // Per-page issues (only pages that have issues)
    let pages_with_issues: Vec<_> = report
        .pages
        .iter()
        .filter(|p| !p.issues.is_empty())
        .collect();

    if pages_with_issues.is_empty() {
        writeln!(out, "## Issues").ok();
        writeln!(out).ok();
        writeln!(out, "No issues found. All pages passed SEO checks.").ok();
    } else {
        writeln!(out, "## Issues by Page").ok();
        writeln!(out).ok();

        for page in &pages_with_issues {
            writeln!(out, "### {} (score: {}/100)", page.url, page.score).ok();
            writeln!(out).ok();

            for issue in &page.issues {
                let severity = match issue.severity {
                    IssueSeverity::Critical => "CRITICAL",
                    IssueSeverity::Warning => "WARNING",
                    IssueSeverity::Info => "INFO",
                };
                writeln!(out, "- [{}] {}: {}", severity, issue.code, issue.message).ok();
                writeln!(out, "  Fix: {}", issue.recommendation).ok();
            }
            writeln!(out).ok();
        }
    }

    // Instruction for the LLM
    writeln!(out, "---").ok();
    writeln!(out).ok();
    writeln!(out, "Based on this SEO audit, please provide:").ok();
    writeln!(out, "1. A prioritized list of the most impactful fixes").ok();
    writeln!(
        out,
        "2. Specific code changes or content recommendations for each issue"
    )
    .ok();
    writeln!(out, "3. Quick wins that can be implemented immediately").ok();
    writeln!(out, "4. Long-term improvements for overall SEO health").ok();

    out
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> SeoReport {
        SeoReport {
            id: "test-123".into(),
            url: "https://example.com".into(),
            score: 72,
            status: ReportStatus::Completed,
            created_at: "2026-02-28T12:00:00Z".into(),
            completed_at: Some("2026-02-28T12:00:05Z".into()),
            duration_ms: 5200,
            summary: ReportSummaryStats {
                pages_crawled: 2,
                total_issues: 3,
                critical: 1,
                warnings: 2,
                info: 0,
                avg_page_score: 72,
                missing_titles: 0,
                missing_descriptions: 1,
                missing_h1: 0,
                images_without_alt: 2,
                missing_canonical: 1,
                missing_og_tags: 1,
            },
            pages: vec![
                PageAnalysis {
                    url: "https://example.com/".into(),
                    status_code: 200,
                    score: 80,
                    title: Some("Example".into()),
                    meta_description: Some("A great site.".into()),
                    canonical: Some("https://example.com/".into()),
                    h1_count: 1,
                    h2_count: 2,
                    image_count: 3,
                    images_without_alt: 2,
                    word_count: 500,
                    internal_links: 10,
                    external_links: 2,
                    has_og_title: true,
                    has_og_description: true,
                    has_og_image: false,
                    has_robots_meta: false,
                    has_viewport: true,
                    has_charset: true,
                    has_lang: true,
                    load_time_ms: 200,
                    issues: vec![
                        SeoIssue {
                            severity: IssueSeverity::Warning,
                            code: "IMAGES_MISSING_ALT".into(),
                            message: "2 of 3 images missing alt text".into(),
                            recommendation: "Add descriptive alt attributes.".into(),
                        },
                        SeoIssue {
                            severity: IssueSeverity::Warning,
                            code: "INCOMPLETE_OG".into(),
                            message: "Missing Open Graph tags: og:image".into(),
                            recommendation: "Add all OG tags.".into(),
                        },
                    ],
                },
                PageAnalysis {
                    url: "https://example.com/about".into(),
                    status_code: 200,
                    score: 65,
                    title: Some("About".into()),
                    meta_description: None,
                    canonical: None,
                    h1_count: 1,
                    h2_count: 0,
                    image_count: 0,
                    images_without_alt: 0,
                    word_count: 300,
                    internal_links: 5,
                    external_links: 1,
                    has_og_title: true,
                    has_og_description: true,
                    has_og_image: true,
                    has_robots_meta: false,
                    has_viewport: true,
                    has_charset: true,
                    has_lang: true,
                    load_time_ms: 150,
                    issues: vec![SeoIssue {
                        severity: IssueSeverity::Critical,
                        code: "MISSING_META_DESC".into(),
                        message: "No meta description found".into(),
                        recommendation: "Add a meta description.".into(),
                    }],
                },
            ],
        }
    }

    #[test]
    fn test_format_report_as_prompt_contains_header() {
        let report = sample_report();
        let prompt = format_report_as_prompt(&report);

        assert!(prompt.contains("# SEO Audit Report"));
        assert!(prompt.contains("Site: https://example.com"));
        assert!(prompt.contains("Overall Score: 72/100"));
        assert!(prompt.contains("Pages Crawled: 2"));
    }

    #[test]
    fn test_format_report_as_prompt_contains_summary() {
        let report = sample_report();
        let prompt = format_report_as_prompt(&report);

        assert!(prompt.contains("- Total Issues: 3"));
        assert!(prompt.contains("- Critical: 1"));
        assert!(prompt.contains("- Warnings: 2"));
        assert!(prompt.contains("- Images without alt text: 2"));
    }

    #[test]
    fn test_format_report_as_prompt_contains_page_issues() {
        let report = sample_report();
        let prompt = format_report_as_prompt(&report);

        // Page headers
        assert!(prompt.contains("### https://example.com/ (score: 80/100)"));
        assert!(prompt.contains("### https://example.com/about (score: 65/100)"));

        // Issue details
        assert!(prompt.contains("[WARNING] IMAGES_MISSING_ALT: 2 of 3 images missing alt text"));
        assert!(prompt.contains("[CRITICAL] MISSING_META_DESC: No meta description found"));
        assert!(prompt.contains("Fix: Add a meta description."));
    }

    #[test]
    fn test_format_report_as_prompt_contains_instructions() {
        let report = sample_report();
        let prompt = format_report_as_prompt(&report);

        assert!(prompt.contains("Based on this SEO audit, please provide:"));
        assert!(prompt.contains("1. A prioritized list"));
    }

    #[test]
    fn test_format_report_as_prompt_no_issues() {
        let report = SeoReport {
            id: "clean".into(),
            url: "https://perfect.com".into(),
            score: 100,
            status: ReportStatus::Completed,
            created_at: "2026-02-28T12:00:00Z".into(),
            completed_at: Some("2026-02-28T12:00:01Z".into()),
            duration_ms: 1000,
            summary: ReportSummaryStats {
                pages_crawled: 1,
                total_issues: 0,
                critical: 0,
                warnings: 0,
                info: 0,
                avg_page_score: 100,
                missing_titles: 0,
                missing_descriptions: 0,
                missing_h1: 0,
                images_without_alt: 0,
                missing_canonical: 0,
                missing_og_tags: 0,
            },
            pages: vec![PageAnalysis {
                url: "https://perfect.com/".into(),
                status_code: 200,
                score: 100,
                title: Some("Perfect".into()),
                meta_description: Some("A perfect page.".into()),
                canonical: Some("https://perfect.com/".into()),
                h1_count: 1,
                h2_count: 2,
                image_count: 1,
                images_without_alt: 0,
                word_count: 500,
                internal_links: 5,
                external_links: 1,
                has_og_title: true,
                has_og_description: true,
                has_og_image: true,
                has_robots_meta: true,
                has_viewport: true,
                has_charset: true,
                has_lang: true,
                load_time_ms: 100,
                issues: vec![],
            }],
        };
        let prompt = format_report_as_prompt(&report);
        assert!(prompt.contains("No issues found. All pages passed SEO checks."));
    }

    #[test]
    fn test_format_report_as_prompt_duration() {
        let report = sample_report();
        let prompt = format_report_as_prompt(&report);
        assert!(prompt.contains("Crawl Duration: 5.2s"));
    }
}
