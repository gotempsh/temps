use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{Json, Redirect},
    routing::{get, post},
    Router,
};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{error, info, warn};
use utoipa::ToSchema;

use super::types::GitAppState as AppState;
use temps_core::problemdetails::{new as problem_new, Problem};
// use crate::services::audit_service::{AuditContext, PipelineTriggeredAudit};
// use crate::services::project::crud::ProjectCrud;
// use crate::services::project::pipelines::ProjectPipelines;

use crate::services::github::GithubAppServiceError;
use octocrab::models::webhook_events::payload::InstallationRepositoriesWebhookEventAction;
use octocrab::models::webhook_events::{EventInstallation, WebhookEvent, WebhookEventPayload};

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        // GITHUB-SPECIFIC endpoints under /git-providers/{provider_id}/github/*
        // Standardized webhook/callback patterns using /webhook/git/{provider}/*
        .route("/webhook/git/github/auth", get(github_app_auth_callback)) // OAuth authorization callback
        .route(
            "/webhook/git/github/callback",
            get(github_app_installation_callback),
        ) // Installation callback
        .route("/webhook/git/github/events", post(github_webhook_events))
        .route("/webhook/git/github/install", get(github_install_webhook))
        // Legacy webhook endpoints (kept for backward compatibility)
        .route("/webhook/github", post(github_webhook))
        .route("/webhook/source/github/events", post(github_webhook_events))
        .route(
            "/webhook/source/github/install",
            get(github_install_webhook),
        )
}

// ===== Webhook Handlers =====

#[derive(ToSchema, Serialize, Deserialize)]
pub struct WebhookResponse {
    message: String,
}

async fn github_webhook(Json(payload): Json<serde_json::Value>) -> Json<WebhookResponse> {
    if let Some(event_type) = payload.get("event_type") {
        if event_type == "push" {
            info!("Received a GitHub push event");
        }
    }
    Json(WebhookResponse {
        message: "GitHub webhook received".to_string(),
    })
}

#[derive(Deserialize)]
#[allow(dead_code)] // Used for OAuth redirect query parsing
struct RedirectQuery {
    code: String,
    state: String,
    source: Option<String>,
}

/// Handle GitHub App manifest conversion with source tracking
/// This is when creating a new GitHub App from a manifest
async fn handle_manifest_conversion_with_source(
    state: &Arc<AppState>,
    code: String,
    source: Option<String>,
    headers: axum::http::HeaderMap,
) -> Result<(HeaderMap, Redirect), Problem> {
    info!(
        "Processing GitHub App manifest conversion with code: {} and source: {:?}",
        code, source
    );

    let client = reqwest::Client::new();
    let api_url = "https://api.github.com";
    let conversions_url = format!("{}/app-manifests/{}/conversions", api_url, code);

    let response = match client
        .post(&conversions_url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "Temps")
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            error!("Failed to convert manifest: {}", e);
            return Err(problem_new(StatusCode::BAD_GATEWAY)
                .with_title("Manifest Conversion Failed")
                .with_detail(format!("Failed to convert manifest: {}", e)));
        }
    };

    let github_app_data = match response.json::<serde_json::Value>().await {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to parse GitHub App response: {}", e);
            return Err(problem_new(StatusCode::BAD_GATEWAY)
                .with_title("Response Parse Failed")
                .with_detail(format!("Failed to parse GitHub App response: {}", e)));
        }
    };

    // Create the GitHub App with source tracking (this creates both github_apps entry and git_provider)
    match state
        .github_service
        .create_github_app(github_app_data, source)
        .await
    {
        Ok(app) => {
            info!(
                "GitHub App created successfully: {} (provider_id: {})",
                app.name, app.provider_id
            );

            // Extract the host for redirect
            let host = headers
                .get("host")
                .and_then(|h| h.to_str().ok())
                .map(|host| {
                    let scheme = headers
                        .get("x-forwarded-proto")
                        .and_then(|p| p.to_str().ok())
                        .unwrap_or("https");
                    format!("{}://{}", scheme, host)
                })
                .unwrap_or_else(|| "http://localhost:8080".to_string());

            // Redirect to the git provider detail page with github_app_created flag
            let redirect_url = format!(
                "{}/git-providers/{}?github_app_created=true",
                host, app.provider_id
            );

            let mut response_headers = HeaderMap::new();
            response_headers.insert("Cache-Control", "no-store".parse().unwrap());

            Ok((response_headers, Redirect::to(&redirect_url)))
        }
        Err(e) => {
            error!("Failed to create GitHub app: {:?}", e);
            Err(problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("GitHub App Creation Failed")
                .with_detail(format!("Failed to create GitHub app: {}", e)))
        }
    }
}

#[derive(ToSchema, Serialize, Deserialize)]
pub struct EventResponse {
    message: String,
}

async fn github_webhook_events(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    axum_body: Bytes,
) -> Result<Json<EventResponse>, Problem> {
    info!("Received GitHub webhook event");
    let body: Vec<u8> = axum_body.to_vec();

    // Validate the webhook signature
    let signature = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok());

    if let Err(e) = state
        .github_service
        .validate_webhook_signature(signature, &body)
        .await
    {
        error!("Invalid webhook signature: {:?}", e);
        return Err(problem_new(StatusCode::UNAUTHORIZED)
            .with_title("Invalid Webhook Signature")
            .with_detail("The webhook signature validation failed"));
    }

    // Get the X-Github-Event header
    let event_type = headers
        .get("X-Github-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    // Parse the webhook event
    let webhook_event = match WebhookEvent::try_from_header_and_body(event_type, &body) {
        Ok(event) => event,
        Err(e) => {
            // Extract additional context from headers for better debugging
            let delivery_id = headers
                .get("X-GitHub-Delivery")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown");

            let github_hook_id = headers
                .get("X-Github-Hook-Id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown");

            let host = headers
                .get("host")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown");

            let user_agent = headers
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown");

            // Try to extract repository info from body if possible
            let (repo_owner, repo_name) =
                if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&body) {
                    let owner = json
                        .get("repository")
                        .and_then(|r| r.get("owner"))
                        .and_then(|o| o.get("login"))
                        .and_then(|l| l.as_str())
                        .unwrap_or("unknown");
                    let name = json
                        .get("repository")
                        .and_then(|r| r.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown");
                    (owner.to_string(), name.to_string())
                } else {
                    ("unknown".to_string(), "unknown".to_string())
                };

            error!(
                "Failed to parse envelope: event_type={:?}, error={:?}, \
                 delivery_id={}, hook_id={}, \
                 repository={}/{}, webhook_size={} bytes, \
                 host={}, user_agent={}",
                event_type,
                e,
                delivery_id,
                github_hook_id,
                repo_owner,
                repo_name,
                body.len(),
                host,
                user_agent
            );
            return Err(problem_new(StatusCode::BAD_REQUEST)
                .with_title("Webhook Parse Failed")
                .with_detail("Failed to parse webhook event"));
        }
    };

    // Process the webhook event based on type
    let installation_res = webhook_event.installation.clone();
    let mut installation_id: i32 = 0;

    match installation_res {
        Some(EventInstallation::Full(install)) => {
            installation_id = install.id.into_inner() as i32;
        }
        Some(EventInstallation::Minimal(installation_)) => {
            installation_id = installation_.id.into_inner() as i32;
        }
        None => {
            warn!("No installation information in webhook event");
        }
    };

    // Handle specific webhook events
    let webhook_event_clone = webhook_event.clone();
    match webhook_event.specific {
        WebhookEventPayload::Push(push_event) => {
            info!("Push event received");
            handle_push_event(&state, webhook_event_clone, push_event, installation_id).await;
        }
        WebhookEventPayload::InstallationRepositories(installation_repos) => {
            info!("Installation repositories event received");
            handle_installation_repositories(&state, installation_repos, installation_id).await;
        }
        WebhookEventPayload::Installation(installation_event) => {
            info!("Installation event received");
            handle_installation_event(&state, installation_event, installation_id).await;
        }
        WebhookEventPayload::Ping(ping_event) => {
            if let Some(app_id) = ping_event.hook.and_then(|h| h.app_id) {
                info!("Ping event app_id: {:?}", app_id);
                let _ = state
                    .github_service
                    .update_ping_received_at(app_id.into_inner() as i32, chrono::Utc::now())
                    .await;
            }
        }
        _ => info!("Received {} event", event_type),
    }

    Ok(Json(EventResponse {
        message: "Webhook event received and processed".to_string(),
    }))
}

async fn handle_push_event(
    state: &Arc<AppState>,
    webhook_event: WebhookEvent,
    push_event: Box<octocrab::models::webhook_events::payload::PushWebhookEventPayload>,
    installation_id: i32,
) {
    let repo = webhook_event.repository.unwrap();
    let repo_owner = repo.owner.unwrap().login;
    let repo_name = repo.name;

    // Check if installation exists
    if state
        .github_service
        .get_installation_by_id(installation_id)
        .await
        .is_err()
    {
        return;
    }

    let git_ref = push_event.r#ref;
    let branch = if git_ref.starts_with("refs/heads/") {
        Some(git_ref.replace("refs/heads/", ""))
    } else {
        push_event
            .base_ref
            .map(|base_ref| base_ref.replace("refs/heads/", ""))
    };

    let tag = if git_ref.starts_with("refs/tags/") {
        Some(git_ref.replace("refs/tags/", ""))
    } else {
        None
    };

    // Use git provider manager to handle the push event
    if let Err(e) = state
        .git_provider_manager
        .handle_push_event(repo_owner, repo_name, branch, tag, push_event.after.clone())
        .await
    {
        error!(
            "Failed to handle push event via git provider manager: {:?}",
            e
        );
    }
}

async fn handle_installation_repositories(
    state: &Arc<AppState>,
    event: Box<
        octocrab::models::webhook_events::payload::InstallationRepositoriesWebhookEventPayload,
    >,
    installation_id: i32,
) {
    match event.action {
        InstallationRepositoriesWebhookEventAction::Added => {
            for repo in &event.repositories_added {
                info!("Repository added: {}", repo.full_name);
                let (owner, name) = match repo.full_name.split_once('/') {
                    Some((o, n)) => (o, n),
                    None => continue,
                };

                if let Ok(db_installation) = state
                    .github_service
                    .get_installation_by_id(installation_id)
                    .await
                {
                    let _ = state
                        .github_service
                        .sync_repository(
                            owner,
                            name,
                            db_installation.github_app_id,
                            installation_id,
                            None,
                        )
                        .await;
                }
            }
        }
        _ => {
            info!("Installation repositories event: {:?}", event.action);
        }
    }
}

async fn handle_installation_event(
    state: &Arc<AppState>,
    event: Box<octocrab::models::webhook_events::payload::InstallationWebhookEventPayload>,
    installation_id: i32,
) {
    use octocrab::models::webhook_events::payload::InstallationWebhookEventAction;

    match event.action {
        InstallationWebhookEventAction::Created => {
            // Process the installation (webhook-only approach)
            // This is the ONLY place where installations are created to avoid duplicates
            // The redirect handlers (/auth and /callback) just redirect and wait for this webhook
            info!(
                "Installation.created webhook received - installation_id: {}",
                installation_id
            );

            // Extract repositories from the webhook payload if available
            let payload_repos = event
                .repositories
                .as_ref()
                .map(|repos| repos.clone())
                .unwrap_or_default();

            if !payload_repos.is_empty() {
                info!(
                    "Webhook payload contains {} repositories - will store them immediately",
                    payload_repos.len()
                );
            }

            match state
                .github_service
                .process_installation(installation_id, None)
                .await
            {
                Ok(result) => {
                    info!(
                        "Successfully processed installation {} via webhook. Result: {}",
                        installation_id, result
                    );

                    // If we have repositories from the webhook payload, store them directly
                    if !payload_repos.is_empty() {
                        info!(
                            "Storing {} repositories from webhook payload for installation {}",
                            payload_repos.len(),
                            installation_id
                        );

                        // Get the connection that was just created
                        if let Ok(_installation_data) = state
                            .github_service
                            .get_installation_by_id(installation_id)
                            .await
                        {
                            for repo in payload_repos {
                                let repo_name = if repo.full_name.is_empty() {
                                    repo.name.clone()
                                } else {
                                    repo.full_name.clone()
                                };

                                match state
                                    .github_service
                                    .store_repository_from_webhook(&repo, installation_id)
                                    .await
                                {
                                    Ok(_) => {
                                        info!(
                                            "Stored repository {} from webhook payload",
                                            repo_name
                                        );
                                    }
                                    Err(e) => {
                                        warn!(
                                            "Failed to store repository {} from webhook payload: {}",
                                            repo_name, e
                                        );
                                    }
                                }
                            }
                        } else {
                            warn!(
                                "Could not find installation {} after creation - repositories from payload were not stored",
                                installation_id
                            );
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "CRITICAL: Failed to process installation {} via webhook. Repositories will not be synced. Error: {:?}",
                        installation_id, e
                    );
                    // Log more details about the error
                    error!(
                        "Installation {} error details - Please check GitHub App configuration and webhook delivery",
                        installation_id
                    );
                }
            }
        }
        InstallationWebhookEventAction::Deleted => {
            info!(
                "Installation.deleted webhook received - installation_id: {}",
                installation_id
            );

            // Delete the installation and all associated repositories
            match state
                .github_service
                .delete_installation(installation_id)
                .await
            {
                Ok(_) => {
                    info!(
                        "Successfully deleted installation {} and associated repositories",
                        installation_id
                    );
                }
                Err(GithubAppServiceError::Conflict(msg)) => {
                    // Installation cannot be deleted due to dependent projects
                    warn!(
                        "Installation {} has dependent projects and cannot be deleted: {}",
                        installation_id, msg
                    );
                }
                Err(e) => {
                    // Log the error but don't fail - the installation may already be deleted
                    warn!(
                        "Failed to delete installation {}: {}. Installation may already be deleted.",
                        installation_id, e
                    );
                }
            }
        }
        _ => {
            info!("Installation event: {:?}", event.action);
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[allow(dead_code)] // Fields logged for debugging GitHub App installation webhooks
struct InstallWebhookParams {
    setup_action: String,
    source: Option<String>,
    installation_id: i32,
}

async fn github_install_webhook(
    state: State<Arc<AppState>>,
    Query(params): Query<InstallWebhookParams>,
) -> Result<Redirect, Problem> {
    info!("Received GitHub install webhook: {:?}", params);
    let settings = state.config_service.get_settings().await.map_err(|e| {
        error!("Failed to get settings: {:?}", e);
        problem_new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Configuration Error")
            .with_detail(format!("Failed to get settings: {}", e))
    })?;
    // Get the external URL from config to construct absolute redirect
    let external_url = settings
        .external_url
        .unwrap_or_else(|| "http://localhost:8080".to_string());

    let redirect_url = format!("{}/dashboard", external_url);
    info!(
        "Redirecting to {} - /api/webhook/source/github/events will handle installation processing",
        redirect_url
    );

    // Redirect to absolute URL to avoid /api/dashboard issue
    Ok(Redirect::to(&redirect_url))
}

/// GitHub App OAuth authorization callback handler
/// This handles both the authorization step and the installation step
#[utoipa::path(
    get,
    path = "/webhook/git/github/auth",
    params(
        ("code" = String, Query, description = "GitHub OAuth authorization code"),
        ("state" = Option<String>, Query, description = "OAuth state parameter"),
        ("installation_id" = Option<i64>, Query, description = "GitHub installation ID (for OAuth flow)"),
        ("setup_action" = Option<String>, Query, description = "Setup action (install, request, or update)")
    ),
    responses(
        (status = 302, description = "Redirect after successful authorization"),
        (status = 400, description = "Bad request - missing or invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers"
)]
/// Helper function to find which GitHub App provider owns a given installation
/// Returns Ok(Some(provider_id)) if found, Ok(None) if not found, Err if error
async fn find_github_app_provider_for_installation(
    state: &Arc<AppState>,
    installation_id: i32,
) -> Result<Option<i32>, String> {
    // Use the github_service to find which provider owns this installation
    // The service will try each GitHub App until it finds one that can access this installation
    match state
        .github_service
        .find_provider_for_installation(installation_id)
        .await
    {
        Ok(provider_id) => Ok(provider_id), // Service already returns Option<i32>
        Err(_) => Ok(None), // If we can't find it, that's ok - webhook will handle it
    }
}

async fn github_app_auth_callback(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> Result<(HeaderMap, Redirect), Problem> {
    let code = params.get("code").cloned().unwrap_or_default();
    let state_param = params.get("state").cloned();
    let installation_id = params
        .get("installation_id")
        .and_then(|id| id.parse::<i64>().ok());
    let setup_action = params.get("setup_action").cloned();

    // Log all parameters for debugging
    info!("GitHub App auth callback - params: {:?}", params);

    if code.is_empty() {
        return Err(problem_new(StatusCode::BAD_REQUEST)
            .with_title("Missing Authorization Code")
            .with_detail("The 'code' parameter is required"));
    }

    // Extract the host from the request headers for consistent redirect URLs
    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .map(|host| {
            let scheme = headers
                .get("x-forwarded-proto")
                .and_then(|p| p.to_str().ok())
                .unwrap_or("https");
            format!("{}://{}", scheme, host)
        });

    // Determine the flow type based on parameters:
    // 1. Manifest flow: Only has 'code' and 'state' parameters
    // 2. OAuth installation flow: Has 'code' + 'installation_id' + 'setup_action'
    // 3. OAuth auth-only flow: Only has 'code' (and possibly 'state')

    // Check if this is an OAuth installation flow (has installation_id)
    // With webhook-only approach, we just redirect and let the webhook handle installation creation
    if let Some(installation_id) = installation_id {
        info!(
            "Detected OAuth installation flow - installation_id: {}, setup_action: {:?}. Installation will be created by webhook.",
            installation_id, setup_action
        );

        // Validate setup_action if provided
        if let Some(action) = &setup_action {
            if action != "install" && action != "request" && action != "update" {
                warn!("Unexpected setup_action: {}", action);
            }
        }

        // Try to find which GitHub App provider owns this installation
        // This helps us redirect to the correct git provider detail page
        let provider_id = match find_github_app_provider_for_installation(
            &state,
            installation_id as i32,
        )
        .await
        {
            Ok(Some(provider_id)) => {
                info!(
                    "Found GitHub App provider {} for installation {}",
                    provider_id, installation_id
                );
                Some(provider_id)
            }
            Ok(None) => {
                warn!("Could not determine GitHub App provider for installation {} - will redirect to git sources", installation_id);
                None
            }
            Err(e) => {
                warn!("Error finding GitHub App provider for installation {}: {} - will redirect to git sources", installation_id, e);
                None
            }
        };

        // Don't process installation here - let the webhook handle it
        // Redirect to git provider detail page with installation info, or git sources if provider not found
        let redirect_url = if let Some(pid) = provider_id {
            format!(
                "{}{}?installation_id={}&github_installation_processing=true",
                host.unwrap_or_else(|| "http://localhost:8080".to_string()),
                format!("/git-providers/{}", pid),
                installation_id
            )
        } else {
            format!(
                "{}{}?installation_id={}&github_installation_processing=true",
                host.unwrap_or_else(|| "http://localhost:8080".to_string()),
                "/git-sources",
                installation_id
            )
        };

        let mut response_headers = HeaderMap::new();
        response_headers.insert("Cache-Control", "no-store".parse().unwrap());

        return Ok((response_headers, Redirect::to(&redirect_url)));
    }

    // Check if this is a manifest conversion flow (only code + state, no installation_id)
    let is_manifest_flow =
        params.len() == 2 && params.contains_key("code") && params.contains_key("state");

    if is_manifest_flow {
        info!(
            "Detected manifest conversion flow with state: {:?}",
            state_param
        );
        // This is a manifest conversion - the code needs to be exchanged for a GitHub App
        return handle_manifest_conversion_with_source(&state, code, state_param, headers).await;
    }

    // For auth-only flow (no installation_id yet), redirect and wait for webhook
    info!("OAuth authorization code received without installation_id - waiting for installation webhook");

    let redirect_url = host.unwrap_or_else(|| "http://localhost:8080".to_string()) + "/dashboard";

    let mut response_headers = HeaderMap::new();
    response_headers.insert("Cache-Control", "no-store".parse().unwrap());

    Ok((response_headers, Redirect::to(&redirect_url)))
}

/// Alternative GitHub App installation callback handler (legacy/fallback)
/// Note: The primary callback is /webhook/git/github/auth which handles both auth and installation
/// This endpoint is kept for backward compatibility or if GitHub sends callbacks to a different URL
#[utoipa::path(
    get,
    path = "/webhook/git/github/callback",
    params(
        ("code" = Option<String>, Query, description = "GitHub OAuth authorization code"),
        ("installation_id" = i64, Query, description = "GitHub installation ID"),
        ("setup_action" = Option<String>, Query, description = "Setup action (install, request, or update)")
    ),
    responses(
        (status = 302, description = "Redirect after successful installation"),
        (status = 400, description = "Bad request - missing or invalid parameters"),
        (status = 500, description = "Internal server error")
    ),
    tag = "Git Providers"
)]
async fn github_app_installation_callback(
    State(_state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> Result<(HeaderMap, Redirect), Problem> {
    // Installation callback may or may not have a code
    let code = params.get("code").cloned();
    let installation_id = params
        .get("installation_id")
        .and_then(|id| id.parse::<i64>().ok());
    let setup_action = params.get("setup_action").cloned();

    // Extract the host from the request headers for consistent callback URL generation
    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .map(|host| {
            let scheme = headers
                .get("x-forwarded-proto")
                .and_then(|p| p.to_str().ok())
                .unwrap_or("https");
            format!("{}://{}/api", scheme, host)
        });

    // Log the received parameters for debugging
    info!(
        "GitHub App callback params - code: {:?}, installation_id: {:?}, setup_action: {:?}",
        code.as_ref().map(|_| "present"),
        installation_id,
        setup_action
    );

    // Installation callback must have installation_id
    if installation_id.is_none() {
        return Err(problem_new(StatusCode::BAD_REQUEST)
            .with_title("Missing Installation ID")
            .with_detail("The 'installation_id' parameter is required for installation callback"));
    }

    let installation_id = installation_id.unwrap();

    // Allow both "install" and "request" as valid setup actions, or no setup_action
    if let Some(action) = &setup_action {
        if action != "install" && action != "request" && action != "update" {
            error!("Invalid setup_action: {}", action);
            return Err(problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Setup Action")
                .with_detail(format!("Setup action '{}' is not supported. Expected 'install', 'request', or 'update'", action)));
        }
    }

    // This is an installation-only callback (auth was done separately)
    // With webhook-only approach, installation will be created by the webhook event
    info!(
        "GitHub App installation callback for installation_id: {} - Installation will be created by webhook",
        installation_id
    );

    // Redirect to dashboard - let the webhook handle installation creation
    let redirect_url = host.unwrap_or_else(|| "http://localhost:8080".to_string()) + "/dashboard";

    let mut response_headers = HeaderMap::new();
    response_headers.insert("Cache-Control", "no-store".parse().unwrap());

    Ok((response_headers, Redirect::to(&redirect_url)))
}
