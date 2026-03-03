//! IndexNow Plugin for Temps
//!
//! Automatically submits URLs to search engines via the IndexNow protocol
//! when deployments succeed. Tracks submission history and suggests which
//! pages need resubmission based on content changes and staleness.

mod crawl;
mod db;
mod indexnow;
mod types;

use axum::body::Body;
use axum::extract::{Json, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::{delete, get, patch, post};
use include_dir::{include_dir, Dir};
use std::sync::Arc;
use temps_plugin_sdk::prelude::*;

use crate::db::{IndexNowStore, StoreError};
use crate::types::*;

/// Embed the web/dist/ directory at compile time.
/// In debug mode without FORCE_WEB_BUILD, this contains a placeholder page.
static UI_DIST: Dir = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

/// Access the embedded UI directory.
pub fn ui_dist() -> &'static Dir<'static> {
    &UI_DIST
}

// ============================================================================
// OpenAPI doc
// ============================================================================

#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "IndexNow",
        version = "0.1.0",
        description = "Submit URLs to search engines instantly when deployments succeed via the IndexNow protocol."
    ),
    paths(
        get_settings,
        update_settings,
        list_submissions,
        delete_submission,
        submit_urls,
        get_suggestions,
    ),
    components(schemas(
        types::PluginSettings,
        types::UpdateSettings,
        types::SubmissionResponse,
        types::SubmissionResult,
        types::SubmitRequest,
        types::SuggestionsRequest,
        types::SuggestionsResponse,
        types::PageSuggestion,
        types::SuggestionReason,
    )),
    tags(
        (name = "Submissions", description = "Manage and trigger IndexNow URL submissions"),
        (name = "Settings",    description = "Plugin configuration"),
    )
)]
struct IndexNowApiDoc;

// ============================================================================
// Plugin definition
// ============================================================================

struct IndexNowPlugin;

impl Default for IndexNowPlugin {
    fn default() -> Self {
        Self
    }
}

/// Shared state accessible from all route handlers.
#[derive(Clone)]
struct AppState {
    store: IndexNowStore,
    http_client: reqwest::Client,
}

impl ExternalPlugin for IndexNowPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::builder("indexnow", "0.1.0")
            .display_name("IndexNow")
            .description("Submit URLs to search engines instantly when deployments succeed")
            .requires_db(false)
            .nav(NavEntry {
                label: "IndexNow".into(),
                icon: "search".into(),
                section: NavSection::Platform,
                path: "/indexnow".into(),
                order: 55,
            })
            .event("deployment.succeeded")
            .build()
    }

    fn router(&self, ctx: PluginContext) -> axum::Router {
        let store = ctx.data_dir().to_path_buf();

        // router() is called from within a tokio async context, so we
        // must use block_in_place to run the async store open without
        // deadlocking the runtime.
        let store = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                IndexNowStore::open(&store)
                    .await
                    .expect("Failed to open IndexNow store")
            })
        });

        let http_client = reqwest::Client::builder()
            .user_agent(PluginSettings::DEFAULT_USER_AGENT)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        let state = Arc::new(AppState { store, http_client });

        axum::Router::new()
            // Settings
            .route("/settings", get(get_settings))
            .route("/settings", patch(update_settings))
            // Submissions
            .route("/submissions", get(list_submissions))
            .route("/submissions", delete(delete_submission))
            // Submit URLs to IndexNow
            .route("/submit", post(submit_urls))
            // Suggestions: which pages need (re)submission
            .route("/suggestions", post(get_suggestions))
            // UI routes — serve the embedded React SPA
            .route("/ui", get(redirect_to_ui))
            .route("/ui/", get(serve_ui_index))
            .route("/ui/{*path}", get(serve_ui_asset))
            // Note: /health is already provided by the SDK runtime — do NOT add it here
            // or axum will panic on Router::merge due to conflicting routes.
            .with_state(state)
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        use utoipa::OpenApi as _;
        Some(IndexNowApiDoc::openapi())
    }

    fn on_start(&self, ctx: &PluginContext) -> Result<(), PluginSdkError> {
        tracing::info!(
            plugin = ctx.plugin_name(),
            data_dir = %ctx.data_dir().display(),
            "IndexNow plugin started"
        );
        Ok(())
    }

    fn on_event(&self, ctx: &PluginContext, event: temps_core::external_plugin::PluginEvent) {
        if event.event_type != "deployment.succeeded" {
            return;
        }

        let url = match event.data.get("url").and_then(|v| v.as_str()) {
            Some(u) if !u.is_empty() => u.to_string(),
            _ => {
                tracing::debug!(
                    event_id = %event.id,
                    "deployment.succeeded event has no URL, skipping IndexNow submission"
                );
                return;
            }
        };

        let deployment_id = event
            .data
            .get("deployment_id")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32);
        let project_id = event
            .data
            .get("project_id")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32);
        let environment_id = event
            .data
            .get("environment_id")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32);

        tracing::info!(
            event_id = %event.id,
            deployment_id = ?deployment_id,
            url = %url,
            "Processing deployment.succeeded for IndexNow auto-submission"
        );

        let data_dir = ctx.data_dir().to_path_buf();

        tokio::spawn(async move {
            if let Err(e) =
                auto_submit_on_deploy(&data_dir, &url, deployment_id, project_id, environment_id)
                    .await
            {
                tracing::error!(
                    error = %e,
                    url = %url,
                    "Auto-submission failed for deployment"
                );
            }
        });
    }
}

// ============================================================================
// Auto-submit on deployment
// ============================================================================

/// Called from on_event when a deployment succeeds.
/// Opens its own store connection (since on_event doesn't have access to AppState).
async fn auto_submit_on_deploy(
    data_dir: &std::path::Path,
    site_url: &str,
    deployment_id: Option<i32>,
    project_id: Option<i32>,
    environment_id: Option<i32>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let store = IndexNowStore::open(data_dir).await?;
    let settings = store.get_settings().await?;

    if !settings.auto_submit {
        tracing::info!("Auto-submit is disabled, skipping");
        return Ok(());
    }

    let api_key = match &settings.api_key {
        Some(key) if !key.is_empty() => key.clone(),
        _ => {
            tracing::warn!("IndexNow API key not configured, skipping auto-submission");
            return Ok(());
        }
    };

    let http_client = reqwest::Client::builder()
        .user_agent(&settings.user_agent)
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    // Discover pages
    tracing::info!(url = %site_url, max_pages = settings.max_pages, "Discovering pages for IndexNow");
    let pages =
        crawl::discover_pages(site_url, settings.max_pages, &settings.user_agent, 15).await?;

    if pages.is_empty() {
        tracing::info!("No pages discovered, nothing to submit");
        return Ok(());
    }

    tracing::info!(
        count = pages.len(),
        "Discovered pages, checking for changes"
    );

    // Determine which pages need submission
    let stale_cutoff =
        chrono::Utc::now() - chrono::Duration::hours(settings.resubmit_after_hours as i64);
    let mut urls_to_submit: Vec<String> = Vec::new();

    for page in &pages {
        let needs_submit = match store.get_submission(&page.url).await? {
            None => true, // Never submitted
            Some(existing) => {
                // Submit if content hash changed or last-modified is newer
                if existing.content_hash.as_deref() != Some(&page.content_hash)
                    || (page.last_modified.is_some()
                        && page.last_modified != existing.last_modified_at)
                {
                    true
                }
                // Check if submission is stale
                else {
                    chrono::DateTime::parse_from_rfc3339(&existing.last_submitted_at)
                        .map(|dt| dt < stale_cutoff)
                        .unwrap_or(true)
                }
            }
        };

        if needs_submit {
            urls_to_submit.push(page.url.clone());
        }
    }

    if urls_to_submit.is_empty() {
        tracing::info!("All pages are fresh, nothing to submit");
        return Ok(());
    }

    // Extract host from site URL
    let host = url::Url::parse(site_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_default();

    tracing::info!(
        count = urls_to_submit.len(),
        host = %host,
        "Submitting changed pages to IndexNow"
    );

    // Submit to IndexNow
    let response = indexnow::submit_urls(
        &http_client,
        &settings.search_engine,
        &api_key,
        &host,
        &urls_to_submit,
    )
    .await?;

    if response.is_success() {
        // Record submissions
        for page in &pages {
            if urls_to_submit.contains(&page.url) {
                store
                    .record_submission(&SubmissionRecord {
                        url: page.url.clone(),
                        host: host.clone(),
                        last_modified_at: page.last_modified.clone(),
                        etag: page.etag.clone(),
                        content_hash: Some(page.content_hash.clone()),
                        last_status_code: Some(page.status_code as i32),
                        deployment_id,
                        project_id,
                        environment_id,
                    })
                    .await?;
            }
        }

        tracing::info!(
            submitted = urls_to_submit.len(),
            api_status = response.status_code,
            "IndexNow auto-submission completed"
        );
    } else {
        tracing::warn!(
            status = response.status_code,
            message = %response.message,
            "IndexNow API returned non-success status"
        );
    }

    Ok(())
}

// ============================================================================
// HTTP Handlers
// ============================================================================

// -- Settings ----------------------------------------------------------------

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
async fn get_settings(
    State(state): State<Arc<AppState>>,
) -> Result<Json<PluginSettings>, AppError> {
    let settings = state.store.get_settings().await?;
    Ok(Json(settings))
}

#[utoipa::path(
    patch,
    path = "/settings",
    tag = "Settings",
    request_body = UpdateSettings,
    responses(
        (status = 200, description = "Updated plugin settings", body = PluginSettings),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
async fn update_settings(
    State(state): State<Arc<AppState>>,
    Json(update): Json<UpdateSettings>,
) -> Result<Json<PluginSettings>, AppError> {
    let settings = state.store.update_settings(&update).await?;
    Ok(Json(settings))
}

// -- Submissions -------------------------------------------------------------

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
struct ListSubmissionsQuery {
    host: Option<String>,
    project_id: Option<i32>,
    limit: Option<u64>,
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
struct DeleteSubmissionQuery {
    url: String,
}

#[utoipa::path(
    get,
    path = "/submissions",
    tag = "Submissions",
    params(ListSubmissionsQuery),
    responses(
        (status = 200, description = "List of URL submissions", body = Vec<SubmissionResponse>),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
async fn list_submissions(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListSubmissionsQuery>,
) -> Result<Json<Vec<SubmissionResponse>>, AppError> {
    let limit = query.limit.unwrap_or(100).min(1000);
    let subs = state
        .store
        .list_submissions(query.host.as_deref(), query.project_id, limit)
        .await?;

    let responses: Vec<SubmissionResponse> = subs
        .into_iter()
        .map(|s| SubmissionResponse {
            url: s.url,
            host: s.host,
            last_submitted_at: s.last_submitted_at,
            last_modified_at: s.last_modified_at,
            submission_count: s.submission_count,
            deployment_id: s.deployment_id,
            project_id: s.project_id,
        })
        .collect();

    Ok(Json(responses))
}

#[utoipa::path(
    delete,
    path = "/submissions",
    tag = "Submissions",
    params(DeleteSubmissionQuery),
    responses(
        (status = 204, description = "Submission deleted"),
        (status = 404, description = "Submission not found"),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
async fn delete_submission(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DeleteSubmissionQuery>,
) -> Result<StatusCode, AppError> {
    let deleted = state.store.delete_submission(&query.url).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Ok(StatusCode::NOT_FOUND)
    }
}

// -- Submit ------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/submit",
    tag = "Submissions",
    request_body = SubmitRequest,
    responses(
        (status = 200, description = "Submission result", body = SubmissionResult),
        (status = 400, description = "Bad request — missing API key or URLs"),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
async fn submit_urls(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SubmitRequest>,
) -> Result<Json<SubmissionResult>, AppError> {
    let settings = state.store.get_settings().await?;

    let api_key = settings
        .api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "IndexNow API key not configured. Set it in settings first.".into(),
            )
        })?;

    // Determine URLs to submit
    let (urls_to_submit, pages_metadata) = if let Some(ref urls) = request.urls {
        // Explicit URLs provided
        (urls.clone(), Vec::new())
    } else if let Some(ref site_url) = request.site_url {
        // Discover pages from the site
        let pages = crawl::discover_pages(site_url, settings.max_pages, &settings.user_agent, 15)
            .await
            .map_err(|e| AppError::Internal(format!("Crawl failed: {}", e)))?;

        let urls: Vec<String> = pages.iter().map(|p| p.url.clone()).collect();
        (urls, pages)
    } else {
        return Err(AppError::BadRequest(
            "Either 'urls' or 'siteUrl' must be provided".into(),
        ));
    };

    if urls_to_submit.is_empty() {
        return Ok(Json(SubmissionResult {
            submitted_count: 0,
            skipped_count: 0,
            failed_count: 0,
            api_status: None,
            error: None,
            submitted_urls: vec![],
        }));
    }

    // Extract host from the first URL
    let host = url::Url::parse(&urls_to_submit[0])
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_default();

    // Submit to IndexNow
    let response = indexnow::submit_urls(
        &state.http_client,
        &settings.search_engine,
        api_key,
        &host,
        &urls_to_submit,
    )
    .await
    .map_err(|e| AppError::Internal(format!("IndexNow submission failed: {}", e)))?;

    let mut result = SubmissionResult {
        submitted_count: urls_to_submit.len(),
        skipped_count: 0,
        failed_count: 0,
        api_status: Some(response.status_code),
        error: if response.is_success() {
            None
        } else {
            Some(response.message.clone())
        },
        submitted_urls: urls_to_submit.clone(),
    };

    if !response.is_success() {
        result.failed_count = urls_to_submit.len();
        result.submitted_count = 0;
        return Ok(Json(result));
    }

    // Record submissions in the store
    for url in &urls_to_submit {
        let page_meta = pages_metadata.iter().find(|p| &p.url == url);
        state
            .store
            .record_submission(&SubmissionRecord {
                url: url.clone(),
                host: host.clone(),
                last_modified_at: page_meta.and_then(|p| p.last_modified.clone()),
                etag: page_meta.and_then(|p| p.etag.clone()),
                content_hash: page_meta.map(|p| p.content_hash.clone()),
                last_status_code: page_meta.map(|p| p.status_code as i32),
                deployment_id: request.deployment_id,
                project_id: request.project_id,
                environment_id: request.environment_id,
            })
            .await?;
    }

    Ok(Json(result))
}

// -- Suggestions -------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/suggestions",
    tag = "Submissions",
    request_body = SuggestionsRequest,
    responses(
        (status = 200, description = "Pages that need (re)submission", body = SuggestionsResponse),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
async fn get_suggestions(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SuggestionsRequest>,
) -> Result<Json<SuggestionsResponse>, AppError> {
    let settings = state.store.get_settings().await?;

    // Discover current pages on the site
    let pages = crawl::discover_pages(
        &request.site_url,
        settings.max_pages,
        &settings.user_agent,
        15,
    )
    .await
    .map_err(|e| AppError::Internal(format!("Crawl failed: {}", e)))?;

    let stale_cutoff =
        chrono::Utc::now() - chrono::Duration::hours(settings.resubmit_after_hours as i64);
    let total_pages_checked = pages.len();
    let mut suggestions: Vec<PageSuggestion> = Vec::new();

    for page in &pages {
        let existing = state.store.get_submission(&page.url).await?;

        let (reason, content_changed) = match &existing {
            None => (SuggestionReason::NeverSubmitted, false),
            Some(sub) => {
                // Check content hash
                if sub.content_hash.as_deref() != Some(&page.content_hash) {
                    (SuggestionReason::ContentHashChanged, true)
                }
                // Check last-modified header
                else if page.last_modified.is_some() && page.last_modified != sub.last_modified_at
                {
                    (SuggestionReason::ContentModified, false)
                }
                // Check staleness
                else if chrono::DateTime::parse_from_rfc3339(&sub.last_submitted_at)
                    .map(|dt| dt < stale_cutoff)
                    .unwrap_or(true)
                {
                    (SuggestionReason::StaleSubmission, false)
                } else {
                    continue; // Page is fresh, skip
                }
            }
        };

        let host = url::Url::parse(&page.url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_string()))
            .unwrap_or_default();

        suggestions.push(PageSuggestion {
            url: page.url.clone(),
            host,
            reason,
            last_submitted_at: existing.as_ref().map(|s| s.last_submitted_at.clone()),
            last_modified_at: existing.as_ref().and_then(|s| s.last_modified_at.clone()),
            current_last_modified: page.last_modified.clone(),
            content_changed,
        });
    }

    let pages_needing_submission = suggestions.len();

    Ok(Json(SuggestionsResponse {
        suggestions,
        total_pages_checked,
        pages_needing_submission,
    }))
}

// ============================================================================
// UI Handlers — serve the embedded React SPA
// ============================================================================

/// Redirect /ui -> /ui/ so relative asset paths work correctly.
async fn redirect_to_ui() -> Response {
    Response::builder()
        .status(StatusCode::MOVED_PERMANENTLY)
        .header(header::LOCATION, "ui/")
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Serve the React SPA index.html.
async fn serve_ui_index() -> Response {
    serve_embedded_file(ui_dist(), "index.html")
}

/// Serve a static asset from the embedded dist/ directory.
/// Falls back to index.html for client-side routing.
async fn serve_ui_asset(Path(path): Path<String>) -> Response {
    let dist = ui_dist();
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
// Error handling
// ============================================================================

enum AppError {
    Store(StoreError),
    BadRequest(String),
    Internal(String),
}

impl From<StoreError> for AppError {
    fn from(e: StoreError) -> Self {
        AppError::Store(e)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            AppError::Store(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };

        let body = serde_json::json!({
            "error": message,
        });

        (status, Json(body)).into_response()
    }
}

// ============================================================================
// Entry point
// ============================================================================

temps_plugin_sdk::main!(IndexNowPlugin);
