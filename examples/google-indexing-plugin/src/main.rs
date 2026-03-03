//! Google Indexing API Plugin for Temps
//!
//! Automatically notifies Google when pages are updated or removed via the
//! Google Indexing API v3. Uses service account authentication (JWT flow).
//! Tracks submission history, quota usage, and supports auto-submission
//! on deployment.succeeded events.

mod auth;
mod db;
mod google_api;
mod types;

use axum::body::Body;
use axum::extract::{Json, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::{delete, get, patch, post};
use include_dir::{include_dir, Dir};
use std::sync::Arc;
use temps_plugin_sdk::prelude::*;

use crate::auth::GoogleAuth;
use crate::db::{GoogleIndexingStore, StoreError};
use crate::google_api::{GoogleApiError, GoogleIndexingClient, NotificationType};
use crate::types::*;

/// Embed the web/dist/ directory at compile time.
static UI_DIST: Dir = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

pub fn ui_dist() -> &'static Dir<'static> {
    &UI_DIST
}

// ============================================================================
// OpenAPI doc
// ============================================================================

#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "Google Indexing",
        version = "0.1.0",
        description = "Notify Google instantly when pages are updated or removed via the Indexing API."
    ),
    paths(
        get_settings,
        update_settings,
        upload_service_account,
        delete_service_account,
        list_submissions,
        delete_submission,
        submit_urls,
        check_url_status,
        get_quota,
    ),
    components(schemas(
        types::PluginSettings,
        types::UpdateSettings,
        types::SubmissionResponse,
        types::SubmissionResult,
        types::UrlSubmissionResult,
        types::SubmitRequest,
        types::CheckStatusRequest,
        types::UrlStatus,
        types::NotificationInfo,
        types::QuotaStatus,
        UploadServiceAccountRequest,
        ServiceAccountInfo,
    )),
    tags(
        (name = "Settings",        description = "Plugin configuration"),
        (name = "Service Account", description = "Upload and manage Google service account credentials"),
        (name = "Submissions",     description = "Manage and trigger Google Indexing API submissions"),
        (name = "Quota",           description = "Monitor daily API quota usage"),
    )
)]
struct GoogleIndexingApiDoc;

// ============================================================================
// Plugin definition
// ============================================================================

struct GoogleIndexingPlugin;

impl Default for GoogleIndexingPlugin {
    fn default() -> Self {
        Self
    }
}

/// Shared state accessible from all route handlers.
#[derive(Clone)]
struct AppState {
    store: GoogleIndexingStore,
    http_client: reqwest::Client,
}

impl AppState {
    /// Build a Google Indexing API client if the service account is configured.
    async fn build_client(&self) -> Result<GoogleIndexingClient, AppError> {
        let key = self.store.get_service_account_key().await?.ok_or_else(|| {
            AppError::BadRequest(
                "Service account not configured. Upload your Google service account key first."
                    .into(),
            )
        })?;

        let auth = GoogleAuth::new(key, self.http_client.clone());
        Ok(GoogleIndexingClient::new(auth, self.http_client.clone()))
    }
}

impl ExternalPlugin for GoogleIndexingPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::builder("google-indexing", "0.1.0")
            .display_name("Google Indexing")
            .description(
                "Notify Google instantly when pages are updated or removed via the Indexing API",
            )
            .requires_db(false)
            .nav(NavEntry {
                label: "Google Indexing".into(),
                icon: "globe".into(),
                section: NavSection::Platform,
                path: "/google-indexing".into(),
                order: 56,
            })
            .event("deployment.succeeded")
            .build()
    }

    fn router(&self, ctx: PluginContext) -> axum::Router {
        let data_dir = ctx.data_dir().to_path_buf();

        let store = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                GoogleIndexingStore::open(&data_dir)
                    .await
                    .expect("Failed to open Google Indexing store")
            })
        });

        let http_client = reqwest::Client::builder()
            .user_agent("TempsBot/1.0 (+https://temps.sh/bot)")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        let state = Arc::new(AppState { store, http_client });

        axum::Router::new()
            // Settings
            .route("/settings", get(get_settings))
            .route("/settings", patch(update_settings))
            // Service account
            .route("/service-account", post(upload_service_account))
            .route("/service-account", delete(delete_service_account))
            // Submissions
            .route("/submissions", get(list_submissions))
            .route("/submissions", delete(delete_submission))
            // Submit URLs to Google
            .route("/submit", post(submit_urls))
            // Check URL status at Google
            .route("/status", post(check_url_status))
            // Quota info
            .route("/quota", get(get_quota))
            // UI routes
            .route("/ui", get(redirect_to_ui))
            .route("/ui/", get(serve_ui_index))
            .route("/ui/{*path}", get(serve_ui_asset))
            .with_state(state)
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        use utoipa::OpenApi as _;
        Some(GoogleIndexingApiDoc::openapi())
    }

    fn on_start(&self, ctx: &PluginContext) -> Result<(), PluginSdkError> {
        tracing::info!(
            plugin = ctx.plugin_name(),
            data_dir = %ctx.data_dir().display(),
            "Google Indexing plugin started"
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
                    "deployment.succeeded event has no URL, skipping Google Indexing submission"
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
            "Processing deployment.succeeded for Google Indexing auto-submission"
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
                    "Google Indexing auto-submission failed for deployment"
                );
            }
        });
    }
}

// ============================================================================
// Auto-submit on deployment
// ============================================================================

async fn auto_submit_on_deploy(
    data_dir: &std::path::Path,
    site_url: &str,
    deployment_id: Option<i32>,
    project_id: Option<i32>,
    environment_id: Option<i32>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let store = GoogleIndexingStore::open(data_dir).await?;
    let settings = store.get_settings().await?;

    if !settings.auto_submit {
        tracing::info!("Auto-submit is disabled, skipping");
        return Ok(());
    }

    let key = match store.get_service_account_key().await? {
        Some(k) => k,
        None => {
            tracing::warn!(
                "Service account not configured, skipping Google Indexing auto-submission"
            );
            return Ok(());
        }
    };

    // Check quota
    let today_usage = store.get_today_usage().await?;
    let remaining = settings.daily_quota.saturating_sub(today_usage);
    if remaining == 0 {
        tracing::warn!(
            daily_quota = settings.daily_quota,
            used_today = today_usage,
            "Daily quota exhausted, skipping auto-submission"
        );
        return Ok(());
    }

    let http_client = reqwest::Client::builder()
        .user_agent("TempsBot/1.0 (+https://temps.sh/bot)")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let auth = GoogleAuth::new(key, http_client.clone());
    let client = GoogleIndexingClient::new(auth, http_client);

    // Submit the deployment URL itself
    let max_to_submit = remaining.min(settings.max_urls_per_deploy);
    let urls = vec![site_url.to_string()];

    tracing::info!(
        url = %site_url,
        remaining_quota = remaining,
        "Submitting deployment URL to Google Indexing API"
    );

    let results = client
        .publish_urls(&urls, NotificationType::URL_UPDATED, max_to_submit)
        .await?;

    let mut submitted = 0usize;
    for result in &results {
        let host = url::Url::parse(&result.url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_string()))
            .unwrap_or_default();

        if result.success {
            submitted += 1;
            store
                .record_submission(&SubmissionRecord {
                    url: result.url.clone(),
                    host,
                    notification_type: "URL_UPDATED".into(),
                    google_response_status: Some(result.status_code as i32),
                    notify_time: result.notify_time.clone(),
                    deployment_id,
                    project_id,
                    environment_id,
                })
                .await?;
        } else {
            tracing::warn!(
                url = %result.url,
                status = result.status_code,
                error = ?result.error,
                "Failed to submit URL to Google Indexing API"
            );
        }
    }

    if submitted > 0 {
        store.increment_quota(submitted).await?;
    }

    tracing::info!(
        submitted = submitted,
        total = results.len(),
        "Google Indexing auto-submission completed"
    );

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

// -- Service account ---------------------------------------------------------

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct UploadServiceAccountRequest {
    /// The full JSON content of the service account key file
    key_json: String,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct ServiceAccountInfo {
    client_email: String,
    project_id: String,
}

#[utoipa::path(
    post,
    path = "/service-account",
    tag = "Service Account",
    request_body = UploadServiceAccountRequest,
    responses(
        (status = 201, description = "Service account key uploaded and validated", body = ServiceAccountInfo),
        (status = 400, description = "Invalid key or authentication failure"),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
async fn upload_service_account(
    State(state): State<Arc<AppState>>,
    Json(request): Json<UploadServiceAccountRequest>,
) -> Result<(StatusCode, Json<ServiceAccountInfo>), AppError> {
    // Validate the key can be parsed
    let key: ServiceAccountKey = serde_json::from_str(&request.key_json)
        .map_err(|e| AppError::BadRequest(format!("Invalid service account key: {}", e)))?;

    // Verify it's a service account type
    if key.key_type != "service_account" {
        return Err(AppError::BadRequest(
            "Key type must be 'service_account'. Download the JSON key from Google Cloud Console."
                .into(),
        ));
    }

    // Test that we can create a JWT (validates the private key)
    let http_client = state.http_client.clone();
    let auth = GoogleAuth::new(key.clone(), http_client);
    let _token_test = auth.get_access_token().await.map_err(|e| {
        AppError::BadRequest(format!(
            "Failed to authenticate with the provided key: {}. Make sure the service account has the Indexing API enabled.",
            e
        ))
    })?;

    // Store the key
    state
        .store
        .set_service_account_key(&request.key_json)
        .await?;

    tracing::info!(
        email = %key.client_email,
        project = %key.project_id,
        "Service account key uploaded successfully"
    );

    Ok((
        StatusCode::CREATED,
        Json(ServiceAccountInfo {
            client_email: key.client_email,
            project_id: key.project_id,
        }),
    ))
}

#[utoipa::path(
    delete,
    path = "/service-account",
    tag = "Service Account",
    responses(
        (status = 204, description = "Service account key deleted"),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
async fn delete_service_account(
    State(state): State<Arc<AppState>>,
) -> Result<StatusCode, AppError> {
    state.store.delete_service_account_key().await?;
    tracing::info!("Service account key deleted");
    Ok(StatusCode::NO_CONTENT)
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
            notification_type: s.notification_type,
            submitted_at: s.submitted_at,
            google_response_status: s.google_response_status,
            notify_time: s.notify_time,
            submission_count: s.submission_count,
            deployment_id: s.deployment_id,
            project_id: s.project_id,
        })
        .collect();

    Ok(Json(responses))
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
struct DeleteSubmissionQuery {
    url: String,
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
        (status = 200, description = "Submission results", body = SubmissionResult),
        (status = 400, description = "No URLs provided or service account not configured"),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
async fn submit_urls(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SubmitRequest>,
) -> Result<Json<SubmissionResult>, AppError> {
    if request.urls.is_empty() {
        return Err(AppError::BadRequest("No URLs provided".into()));
    }

    let settings = state.store.get_settings().await?;
    let today_usage = state.store.get_today_usage().await?;
    let remaining = settings.daily_quota.saturating_sub(today_usage);

    if remaining == 0 {
        return Ok(Json(SubmissionResult {
            submitted_count: 0,
            skipped_count: request.urls.len(),
            failed_count: 0,
            results: vec![],
            error: Some(format!(
                "Daily quota exhausted ({}/{}). Quota resets at midnight UTC.",
                today_usage, settings.daily_quota
            )),
            remaining_quota: 0,
        }));
    }

    let client = state.build_client().await?;
    let notification_type = request
        .notification_type
        .as_deref()
        .map(NotificationType::from_str_loose)
        .unwrap_or(NotificationType::URL_UPDATED);

    let max_to_submit = remaining.min(request.urls.len());
    let api_results = client
        .publish_urls(&request.urls, notification_type, max_to_submit)
        .await
        .map_err(|e| AppError::Internal(format!("Google API error: {}", e)))?;

    let mut submitted_count = 0usize;
    let mut failed_count = 0usize;
    let mut url_results = Vec::with_capacity(api_results.len());

    for result in &api_results {
        let host = url::Url::parse(&result.url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_string()))
            .unwrap_or_default();

        if result.success {
            submitted_count += 1;
            state
                .store
                .record_submission(&SubmissionRecord {
                    url: result.url.clone(),
                    host,
                    notification_type: notification_type.to_string(),
                    google_response_status: Some(result.status_code as i32),
                    notify_time: result.notify_time.clone(),
                    deployment_id: request.deployment_id,
                    project_id: request.project_id,
                    environment_id: request.environment_id,
                })
                .await?;
        } else {
            failed_count += 1;
        }

        url_results.push(UrlSubmissionResult {
            url: result.url.clone(),
            success: result.success,
            status_code: Some(result.status_code),
            error: result.error.clone(),
            notify_time: result.notify_time.clone(),
        });
    }

    if submitted_count > 0 {
        state.store.increment_quota(submitted_count).await?;
    }

    let skipped_count = request.urls.len().saturating_sub(api_results.len());

    Ok(Json(SubmissionResult {
        submitted_count,
        skipped_count,
        failed_count,
        results: url_results,
        error: None,
        remaining_quota: remaining.saturating_sub(submitted_count),
    }))
}

// -- Check URL status --------------------------------------------------------

#[utoipa::path(
    post,
    path = "/status",
    tag = "Submissions",
    request_body = CheckStatusRequest,
    responses(
        (status = 200, description = "URL notification status at Google", body = UrlStatus),
        (status = 400, description = "Service account not configured"),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
async fn check_url_status(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CheckStatusRequest>,
) -> Result<Json<UrlStatus>, AppError> {
    let client = state.build_client().await?;

    let metadata = client
        .get_url_metadata(&request.url)
        .await
        .map_err(|e| AppError::Internal(format!("Google API error: {}", e)))?;

    Ok(Json(UrlStatus {
        url: metadata.url.unwrap_or_else(|| request.url.clone()),
        latest_update: metadata.latest_update.map(|u| NotificationInfo {
            url: u.url.unwrap_or_default(),
            notification_type: u.notification_type.unwrap_or_default(),
            notify_time: u.notify_time.unwrap_or_default(),
        }),
        latest_remove: metadata.latest_remove.map(|r| NotificationInfo {
            url: r.url.unwrap_or_default(),
            notification_type: r.notification_type.unwrap_or_default(),
            notify_time: r.notify_time.unwrap_or_default(),
        }),
    }))
}

// -- Quota -------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/quota",
    tag = "Quota",
    responses(
        (status = 200, description = "Current daily quota status", body = QuotaStatus),
        (status = 500, description = "Internal server error"),
    ),
    security(())
)]
async fn get_quota(State(state): State<Arc<AppState>>) -> Result<Json<QuotaStatus>, AppError> {
    let settings = state.store.get_settings().await?;
    let used_today = state.store.get_today_usage().await?;

    // Try to get the real quota limit from Google Service Usage API.
    // Falls back to the locally configured limit if the API call fails
    // (e.g., missing serviceusage.quotas.get permission).
    let daily_limit = match state.build_client().await {
        Ok(client) => match client.get_real_quota_limit().await {
            Ok(Some(limit)) => {
                let real_limit = limit as usize;
                // If the real limit differs from stored setting, update it
                if real_limit != settings.daily_quota {
                    tracing::info!(
                        old = settings.daily_quota,
                        new = real_limit,
                        "Updated daily quota from Google Service Usage API"
                    );
                    let _ = state
                        .store
                        .update_settings(&UpdateSettings {
                            daily_quota: Some(real_limit),
                            ..Default::default()
                        })
                        .await;
                }
                real_limit
            }
            Ok(None) => {
                tracing::debug!("Could not determine real quota from Google, using local setting");
                settings.daily_quota
            }
            Err(e) => {
                tracing::debug!(error = %e, "Failed to query Google quota, using local setting");
                settings.daily_quota
            }
        },
        Err(_) => settings.daily_quota, // No service account configured
    };

    // Quota resets at midnight UTC
    let now = chrono::Utc::now();
    let tomorrow = (now + chrono::Duration::days(1))
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let resets_at =
        chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(tomorrow, chrono::Utc)
            .to_rfc3339();

    Ok(Json(QuotaStatus {
        daily_limit,
        used_today,
        remaining: daily_limit.saturating_sub(used_today),
        resets_at,
    }))
}

// ============================================================================
// UI Handlers
// ============================================================================

async fn redirect_to_ui() -> Response {
    Response::builder()
        .status(StatusCode::MOVED_PERMANENTLY)
        .header(header::LOCATION, "ui/")
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn serve_ui_index() -> Response {
    serve_embedded_file(ui_dist(), "index.html")
}

async fn serve_ui_asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    let dist = ui_dist();
    if dist.get_file(&path).is_some() {
        return serve_embedded_file(dist, &path);
    }
    serve_embedded_file(dist, "index.html")
}

fn serve_embedded_file(dist: &Dir<'static>, path: &str) -> Response {
    match dist.get_file(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path)
                .first_or_octet_stream()
                .to_string();

            let cache = if path == "index.html" {
                "no-cache"
            } else {
                "public, max-age=31536000, immutable"
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

impl From<GoogleApiError> for AppError {
    fn from(e: GoogleApiError) -> Self {
        AppError::Internal(e.to_string())
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

temps_plugin_sdk::main!(GoogleIndexingPlugin);
