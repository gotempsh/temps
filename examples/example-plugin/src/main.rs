//! SEO Analyzer — an example external plugin for Temps.
//!
//! Crawls deployed sites, analyzes pages for technical SEO issues, and
//! generates actionable reports with scores. Demonstrates realistic use
//! of the plugin SDK: HTTP routes, SQLite persistence, background analysis,
//! and a React-based UI embedded at compile time.
//!
//! ## Features
//!
//! - **Site crawling**: Follows internal links with configurable depth
//! - **Technical SEO checks**: Title, meta description, headings, images,
//!   canonical URLs, Open Graph tags, robots directives, and more
//! - **Scoring**: Per-page and per-report aggregate scores (0–100)
//! - **Issue classification**: Critical / Warning / Info severity levels
//! - **SQLite persistence**: Reports survive plugin restarts
//! - **Configurable**: Max pages, crawl delay, user-agent, timeout — all
//!   adjustable via the settings API
//! - **React UI**: Full React + TypeScript frontend embedded at compile time
//!
//! ## API
//!
//! ```text
//! POST   /analyze              — Start a new analysis (body: { "url": "...", "max_pages": 100 })
//! GET    /reports               — List all reports
//! GET    /reports/{id}          — Get a single report with page-level details
//! GET    /reports/{id}/prompt   — Get report as LLM-friendly plain text (text/plain)
//! DELETE /reports/{id}          — Delete a report
//! GET    /settings              — Get plugin settings
//! PATCH  /settings              — Update plugin settings (partial)
//! GET    /ui/                   — Plugin UI (React SPA, served in Temps iframe)
//! GET    /ui/{*path}            — Plugin UI static assets
//! ```
//!
//! ## Development
//!
//! ```bash
//! # Run the React dev server (hot reload)
//! cd examples/example-plugin/web && bun install && bun run dev
//!
//! # Build the plugin binary (skips web build in debug mode)
//! cargo build -p temps-example-plugin
//!
//! # Build with embedded UI
//! FORCE_WEB_BUILD=1 cargo build -p temps-example-plugin
//! ```

mod crawl;
mod db;
mod handlers;
mod types;

use std::sync::OnceLock;

use axum::routing::{get, post};
use include_dir::{include_dir, Dir};
use temps_plugin_sdk::prelude::*;

use crate::crawl::CrawlConfig;
use crate::db::SeoStore;
use crate::handlers::AppState;

/// Embed the web/dist/ directory at compile time.
/// In debug mode without FORCE_WEB_BUILD, this contains a placeholder page.
static UI_DIST: Dir = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

/// Access the embedded UI directory (used by handlers module).
pub fn ui_dist() -> &'static Dir<'static> {
    &UI_DIST
}

// ============================================================================
// OpenAPI doc
// ============================================================================

#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "SEO Analyzer",
        version = "0.1.0",
        description = "Crawl deployed sites and generate technical SEO reports with actionable insights."
    ),
    paths(
        handlers::start_analysis,
        handlers::list_reports,
        handlers::get_report,
        handlers::delete_report,
        handlers::get_report_prompt,
        handlers::get_settings,
        handlers::update_settings,
    ),
    components(schemas(
        types::AnalyzeRequest,
        types::AnalyzeResponse,
        types::ReportSummary,
        types::ReportStatus,
        types::SeoReport,
        types::ReportSummaryStats,
        types::PageAnalysis,
        types::SeoIssue,
        types::IssueSeverity,
        types::PluginSettings,
        types::UpdateSettings,
    )),
    tags(
        (name = "SEO Analysis", description = "Start and manage crawl analyses"),
        (name = "SEO Reports",  description = "Retrieve and delete SEO reports"),
        (name = "Settings",     description = "Plugin configuration"),
    )
)]
struct SeoApiDoc;

// ============================================================================
// Plugin Definition
// ============================================================================

struct SeoPlugin {
    /// Shared app state, initialized once in `router()`.
    /// Used by `on_event` to trigger background crawls without
    /// duplicating the SeoStore and HTTP client setup.
    app_state: OnceLock<AppState>,
}

impl Default for SeoPlugin {
    fn default() -> Self {
        Self {
            app_state: OnceLock::new(),
        }
    }
}

impl ExternalPlugin for SeoPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::builder("seo-analyzer", "0.1.0")
            .display_name("SEO Analyzer")
            .description(
                "Crawl deployed sites and generate technical SEO reports with actionable insights",
            )
            .requires_db(false)
            .nav(NavEntry {
                label: "SEO Reports".into(),
                icon: "search".into(),
                section: NavSection::Platform,
                path: "/seo-reports".into(),
                order: 42,
            })
            .event("deployment.succeeded")
            .build()
    }

    fn router(&self, ctx: PluginContext) -> axum::Router {
        // router() is sync but called inside the tokio runtime during startup.
        // Use block_in_place to run the async SQLite init without deadlocking.
        let store = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(SeoStore::open(ctx.data_dir()))
        })
        .expect("Failed to open SEO store");

        // Build HTTP client using plugin settings defaults.
        // Settings are loaded lazily per-request for the crawl itself,
        // but the client timeout is set here.
        let http_client = reqwest::Client::builder()
            .user_agent(types::PluginSettings::DEFAULT_USER_AGENT)
            .timeout(std::time::Duration::from_secs(
                types::PluginSettings::DEFAULT_REQUEST_TIMEOUT_SECS,
            ))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap_or_default();

        let state = AppState { store, http_client };

        // Store a copy for on_event access
        let _ = self.app_state.set(state.clone());

        axum::Router::new()
            // API routes
            .route("/analyze", post(handlers::start_analysis))
            .route("/reports", get(handlers::list_reports))
            .route(
                "/reports/{id}",
                get(handlers::get_report).delete(handlers::delete_report),
            )
            .route("/reports/{id}/prompt", get(handlers::get_report_prompt))
            .route(
                "/settings",
                get(handlers::get_settings).patch(handlers::update_settings),
            )
            // UI routes — serve the embedded React SPA
            .route("/ui", get(handlers::redirect_to_ui))
            .route("/ui/", get(handlers::serve_ui_index))
            .route("/ui/{*path}", get(handlers::serve_ui_asset))
            .with_state(state)
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        use utoipa::OpenApi as _;
        Some(SeoApiDoc::openapi())
    }

    fn on_start(&self, ctx: &PluginContext) -> Result<(), PluginSdkError> {
        tracing::info!(
            plugin = ctx.plugin_name(),
            data_dir = %ctx.data_dir().display(),
            "SEO Analyzer plugin started"
        );
        Ok(())
    }

    fn on_event(&self, _ctx: &PluginContext, event: temps_core::external_plugin::PluginEvent) {
        if event.event_type != "deployment.succeeded" {
            return;
        }

        let url = match event.data.get("url").and_then(|v| v.as_str()) {
            Some(u) if !u.is_empty() => u.to_string(),
            _ => {
                tracing::debug!(
                    event_id = %event.id,
                    "deployment.succeeded event has no URL, skipping auto-analysis"
                );
                return;
            }
        };

        let Some(state) = self.app_state.get().cloned() else {
            tracing::warn!(
                event_id = %event.id,
                "Cannot run auto-analysis: plugin app state not initialized yet"
            );
            return;
        };

        let deployment_id = event.data.get("deployment_id").and_then(|v| v.as_i64());
        let environment_name = event
            .data
            .get("environment_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        tracing::info!(
            event_id = %event.id,
            deployment_id = ?deployment_id,
            environment = %environment_name,
            url = %url,
            "Starting automatic SEO analysis for successful deployment"
        );

        // Spawn background crawl using the same store and HTTP client
        // that the manual /analyze endpoint uses.
        tokio::spawn(async move {
            // The deployment.succeeded event is only emitted after the proxy has
            // confirmed that the new routes are loaded (route-ready guarantee),
            // so no artificial delay is needed here.

            let report_id = uuid::Uuid::new_v4().to_string();

            let settings = match state.store.get_settings().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "Failed to load settings for auto-analysis");
                    return;
                }
            };

            if let Err(e) = state.store.create_report(&report_id, &url).await {
                tracing::error!(
                    report_id = %report_id,
                    url = %url,
                    error = %e,
                    "Failed to create auto-analysis report"
                );
                return;
            }

            let crawl_delay = std::time::Duration::from_millis(settings.crawl_delay_ms);

            crawl::run_analysis(
                &state.store,
                &state.http_client,
                &report_id,
                &url,
                CrawlConfig {
                    max_pages: settings.default_max_pages,
                    crawl_delay,
                },
            )
            .await;

            tracing::info!(
                report_id = %report_id,
                url = %url,
                deployment_id = ?deployment_id,
                "Automatic SEO analysis completed for deployment"
            );
        });
    }
}

temps_plugin_sdk::main!(SeoPlugin);
