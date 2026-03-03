//! Lighthouse Performance Audit — an external plugin for Temps.
//!
//! Runs automated Lighthouse audits after deployments succeed and tracks
//! Core Web Vitals (LCP, FCP, TBT, CLS) over time. Supports manual audits
//! and configurable score thresholds for regression alerts.
//!
//! ## Features
//!
//! - **Automatic audits**: Subscribes to `deployment.succeeded` and
//!   `deployment.ready` events — when a deployment goes live, Lighthouse
//!   runs against the deployed URL automatically.
//! - **Manual audits**: Run Lighthouse against any URL via API or UI.
//! - **Core Web Vitals tracking**: LCP, FCP, TBT, CLS, Speed Index, TTI
//! - **Score history**: Track Performance, Accessibility, Best Practices,
//!   and SEO scores over time with visual charts.
//! - **Diagnostics**: Surface actionable Lighthouse opportunities and
//!   diagnostics with severity classification.
//! - **Regression alerts**: Configurable score threshold — scores below
//!   the threshold are flagged.
//! - **Raw JSON**: Full Lighthouse JSON output stored for deep analysis.
//! - **Device emulation**: Mobile (default) or Desktop audits.
//!
//! ## API
//!
//! ```text
//! POST   /audit                — Start a manual audit (body: { "url": "..." })
//! GET    /audits               — List all audits
//! GET    /audits/{id}          — Get a single audit with details
//! GET    /audits/{id}/raw      — Get raw Lighthouse JSON
//! DELETE /audits/{id}          — Delete an audit
//! GET    /history              — Score history for charts
//! GET    /status               — Check if Lighthouse CLI is available
//! GET    /settings             — Get plugin settings
//! PATCH  /settings             — Update plugin settings
//! GET    /ui/                  — Plugin UI (React SPA)
//! GET    /ui/{*path}           — Plugin UI static assets
//! ```
//!
//! ## Requirements
//!
//! - Google Chrome or Chromium installed on the host
//! - Lighthouse CLI: `npm install -g lighthouse`
//!
//! ## Development
//!
//! ```bash
//! cd examples/lighthouse-plugin/web && bun install && bun run dev
//! cargo build -p temps-lighthouse-plugin
//! ```

mod db;
mod handlers;
mod lighthouse;
mod types;

use axum::routing::{get, post};
use include_dir::{include_dir, Dir};
use temps_plugin_sdk::prelude::*;

use crate::db::AuditStore;
use crate::handlers::AppState;

// ============================================================================
// OpenAPI doc
// ============================================================================

#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "Lighthouse Audits",
        version = "0.1.0",
        description = "Automated Lighthouse performance audits — track Core Web Vitals, accessibility, and SEO scores over time."
    ),
    paths(
        handlers::start_audit,
        handlers::list_audits,
        handlers::get_audit,
        handlers::delete_audit,
        handlers::get_raw_json,
        handlers::get_score_history,
        handlers::get_status,
        handlers::get_settings,
        handlers::update_settings,
    ),
    components(schemas(
        types::AuditRequest,
        types::StartAuditResponse,
        types::AuditSummary,
        types::LighthouseAudit,
        types::AuditStatus,
        types::AuditTrigger,
        types::CoreWebVitals,
        types::AuditDiagnostic,
        types::DiagnosticSeverity,
        types::ScoreHistoryPoint,
        types::StatusResponse,
        types::PluginSettings,
        types::UpdateSettings,
    )),
    tags(
        (name = "Audits",   description = "Start, list, and manage Lighthouse audits"),
        (name = "Settings", description = "Plugin configuration"),
    )
)]
struct LighthouseApiDoc;

/// Embed the web/dist/ directory at compile time.
static UI_DIST: Dir = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

/// Access the embedded UI directory.
pub fn ui_dist() -> &'static Dir<'static> {
    &UI_DIST
}

// ============================================================================
// Plugin Definition
// ============================================================================

#[derive(Default)]
struct LighthousePlugin;

impl ExternalPlugin for LighthousePlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::builder("lighthouse", "0.1.0")
            .display_name("Lighthouse Audits")
            .description(
                "Automated Lighthouse performance audits after deployments — track Core Web Vitals, accessibility, and SEO scores over time",
            )
            .requires_db(false)
            .nav(NavEntry {
                label: "Lighthouse".into(),
                icon: "gauge".into(),
                section: NavSection::Platform,
                path: "/lighthouse".into(),
                order: 43,
            })
            .event("deployment.succeeded")
            .event("deployment.ready")
            .build()
    }

    fn router(&self, ctx: PluginContext) -> axum::Router {
        let store = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(AuditStore::open(ctx.data_dir()))
        })
        .expect("Failed to open Lighthouse audit store");

        let state = AppState { store };

        axum::Router::new()
            // API routes
            .route("/audit", post(handlers::start_audit))
            .route("/audits", get(handlers::list_audits))
            .route(
                "/audits/{id}",
                get(handlers::get_audit).delete(handlers::delete_audit),
            )
            .route("/audits/{id}/raw", get(handlers::get_raw_json))
            .route("/history", get(handlers::get_score_history))
            .route("/status", get(handlers::get_status))
            .route(
                "/settings",
                get(handlers::get_settings).patch(handlers::update_settings),
            )
            // UI routes
            .route("/ui", get(handlers::redirect_to_ui))
            .route("/ui/", get(handlers::serve_ui_index))
            .route("/ui/{*path}", get(handlers::serve_ui_asset))
            .with_state(state)
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        use utoipa::OpenApi as _;
        Some(LighthouseApiDoc::openapi())
    }

    fn on_start(&self, ctx: &PluginContext) -> Result<(), PluginSdkError> {
        tracing::info!(
            plugin = ctx.plugin_name(),
            data_dir = %ctx.data_dir().display(),
            "Lighthouse Audits plugin started"
        );
        Ok(())
    }

    fn on_event(&self, ctx: &PluginContext, event: PluginEvent) {
        tracing::info!(
            plugin = ctx.plugin_name(),
            event_type = %event.event_type,
            project_id = ?event.project_id,
            "Received platform event"
        );

        // Extract URL from deployment event data
        let url = event
            .data
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let Some(url) = url else {
            tracing::debug!(
                event_type = %event.event_type,
                "No URL in event data, skipping audit"
            );
            return;
        };

        let project_id = event.project_id;
        let deployment_id = event
            .data
            .get("deployment_id")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32);

        let data_dir = ctx.data_dir().to_path_buf();

        // Spawn the audit in a background task.
        // We need to open a fresh store because on_event is sync.
        tokio::spawn(async move {
            let store = match AuditStore::open(&data_dir).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to open store for deployment audit: {}", e);
                    return;
                }
            };

            let settings = match store.get_settings().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to load settings for deployment audit: {}", e);
                    return;
                }
            };

            if !settings.auto_audit_on_deploy {
                tracing::debug!("Auto-audit disabled, skipping deployment audit");
                return;
            }

            let audit_id = uuid::Uuid::new_v4().to_string();

            if let Err(e) = store
                .create_audit(
                    &audit_id,
                    &url,
                    &types::AuditTrigger::Deployment,
                    project_id,
                    deployment_id,
                    &settings.device,
                )
                .await
            {
                tracing::error!(
                    audit_id = %audit_id,
                    url = %url,
                    error = %e,
                    "Failed to create deployment audit"
                );
                return;
            }

            tracing::info!(
                audit_id = %audit_id,
                url = %url,
                project_id = ?project_id,
                deployment_id = ?deployment_id,
                "Starting deployment-triggered Lighthouse audit"
            );

            handlers::run_audit_background(&store, &audit_id, &url, &settings, None, None).await;
        });
    }
}

temps_plugin_sdk::main!(LighthousePlugin);
