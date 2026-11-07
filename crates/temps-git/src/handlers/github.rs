// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    extract::{DefaultBodyLimit, Query, State},
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
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::{new as problem_new, Problem};
use temps_core::telemetry::{OperationFailureCode, TelemetryEvent, TelemetryEventKind};
// use crate::services::audit_service::{AuditContext, PipelineTriggeredAudit};
// use crate::services::project::crud::ProjectCrud;
// use crate::services::project::pipelines::ProjectPipelines;

use crate::services::github::GithubAppServiceError;
use octocrab::models::webhook_events::payload::InstallationRepositoriesWebhookEventAction;
use octocrab::models::webhook_events::{EventInstallation, WebhookEvent, WebhookEventPayload};

/// Browser-driven GitHub App callbacks. GitHub redirects the operator's own
/// browser to these, so they stay on the session-bearing console surface.
pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        // GITHUB-SPECIFIC endpoints under /git-providers/{provider_id}/github/*
        // Standardized webhook/callback patterns using /webhook/git/{provider}/*
        .route("/webhook/git/github/auth", get(github_app_auth_callback)) // OAuth authorization callback
        .route(
            "/webhook/git/github/callback",
            get(github_app_installation_callback),
        ) // Installation callback
        .route("/webhook/git/github/install", get(github_install_webhook))
        .route(
            "/webhook/source/github/install",
            get(github_install_webhook),
        )
}

/// The server-to-server event receiver GitHub delivers webhooks to, on the
/// current path and the legacy `/webhook/source` one. Authenticated by
/// `X-Hub-Signature-256` against the App's webhook secret before any event is
/// processed, so it needs no session and is mounted on the public router.
pub fn configure_public_routes() -> Router<Arc<AppState>> {
    let events = || {
        post(github_webhook_events).layer(DefaultBodyLimit::max(super::PROVIDER_EVENT_BODY_LIMIT))
    };
    Router::new()
        .route("/webhook/git/github/events", events())
        .route("/webhook/source/github/events", events())
        .route(
            GITHUB_REPO_WEBHOOK_PATH,
            post(github_repo_webhook_events)
                .layer(DefaultBodyLimit::max(super::PROVIDER_EVENT_BODY_LIMIT)),
        )
}

/// Where a per-project GitHub *repository* webhook delivers (as opposed to a
/// GitHub App's webhook, which uses `/webhook/git/github/events`). Temps
/// registers one for each project on a non-App GitHub connection (PAT or
/// OAuth), signed with that project's own secret.
pub const GITHUB_REPO_WEBHOOK_PATH: &str = "/webhook/git/github/repo-events";

// ===== Webhook Handlers =====

/// Handle GitHub App manifest conversion with source tracking
/// This is when creating a new GitHub App from a manifest
async fn handle_manifest_conversion_with_source(
    state: &Arc<AppState>,
    code: String,
    source: Option<String>,
) -> Result<(HeaderMap, Redirect), Problem> {
    // The code is a one-time credential: it is not logged.
    info!(
        "Processing GitHub App manifest conversion (source: {:?})",
        source
    );

    let github_app_data = fetch_manifest_app(GITHUB_API_URL, &code).await?;

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

            // Use the operator-configured external URL — never reflect client headers.
            let external_url = state
                .config_service
                .get_external_url()
                .await
                .map_err(|e| {
                    problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("Settings Unavailable")
                        .with_detail(format!("Failed to load external URL: {}", e))
                })?
                .ok_or_else(|| {
                    problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("External URL Not Configured")
                        .with_detail(
                            "Server external_url must be configured before completing GitHub App installation",
                        )
                })?;
            let external_url = external_url.trim_end_matches('/');

            // Redirect to the git provider detail page with github_app_created flag
            let redirect_url = format!(
                "{}/git-providers/{}?github_app_created=true",
                external_url, app.provider_id
            );

            let mut response_headers = HeaderMap::new();
            response_headers.insert(
                "Cache-Control",
                axum::http::HeaderValue::from_static("no-store"),
            );

            Ok((response_headers, Redirect::to(&redirect_url)))
        }
        Err(e) => Err(app_creation_problem(e)),
    }
}

/// The response for a failure to create the GitHub App from GitHub's reply.
///
/// Every variant is listed, so adding one forces a decision about its status.
/// An unusable reply is GitHub's fault (502), an app that is already
/// registered is a conflict (409), and everything else stays a 500.
fn app_creation_problem(error: GithubAppServiceError) -> Problem {
    match error {
        GithubAppServiceError::Validation(message) => {
            warn!(error = %message, "GitHub returned an unusable app");
            ErrorBuilder::new(StatusCode::BAD_GATEWAY)
                .title("Incomplete GitHub App")
                .detail(message)
                .build()
        }
        GithubAppServiceError::Conflict(message) => {
            warn!(error = %message, "GitHub App already registered");
            ErrorBuilder::new(StatusCode::CONFLICT)
                .title("GitHub App Already Registered")
                .detail(message)
                .build()
        }
        error @ (GithubAppServiceError::DatabaseError(_)
        | GithubAppServiceError::GithubApiError(_)
        | GithubAppServiceError::NotFound(_)
        | GithubAppServiceError::DecryptionFailed(_)
        | GithubAppServiceError::EncryptionFailed(_)
        | GithubAppServiceError::PrivateKeyCreationFailed(_)
        | GithubAppServiceError::InstallationNotFound
        | GithubAppServiceError::InvalidWebhookSignature
        | GithubAppServiceError::Unauthorized(_)
        | GithubAppServiceError::InvalidConfiguration(_)
        | GithubAppServiceError::Other(_)) => {
            error!(error = ?error, "Failed to create GitHub app");
            ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("GitHub App Creation Failed")
                .detail(format!("Failed to create GitHub app: {}", error))
                .build()
        }
    }
}

const GITHUB_API_URL: &str = "https://api.github.com";

/// Exchange a manifest `code` for the GitHub App it created.
///
/// Returns the JSON body only when GitHub answers with a success status. The
/// code is percent-encoded into the request path, and reqwest errors are
/// logged and returned without their URL, which would contain the code.
async fn fetch_manifest_app(api_url: &str, code: &str) -> Result<serde_json::Value, Problem> {
    let conversions_url = format!(
        "{}/app-manifests/{}/conversions",
        api_url,
        urlencoding::encode(code)
    );

    let response = reqwest::Client::new()
        .post(&conversions_url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "Temps")
        .send()
        .await
        .map_err(|e| {
            let e = e.without_url();
            error!(error = %e, "Failed to convert manifest");
            ErrorBuilder::new(StatusCode::BAD_GATEWAY)
                .title("Manifest Conversion Failed")
                .detail(format!("Failed to convert manifest: {}", e))
                .build()
        })?;

    let status = response.status();
    if !status.is_success() {
        warn!(
            github_status = status.as_u16(),
            "GitHub rejected the manifest conversion"
        );
        return Err(ErrorBuilder::new(StatusCode::BAD_GATEWAY)
            .title("Manifest Conversion Failed")
            .detail(format!(
                "GitHub answered the manifest conversion with HTTP {}",
                status.as_u16()
            ))
            .build());
    }

    response.json::<serde_json::Value>().await.map_err(|e| {
        let e = e.without_url();
        error!(error = %e, "Failed to parse GitHub App response");
        ErrorBuilder::new(StatusCode::BAD_GATEWAY)
            .title("Response Parse Failed")
            .detail(format!("Failed to parse GitHub App response: {}", e))
            .build()
    })
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

    let signing = state
        .github_service
        .validate_webhook_signature(signature, &body)
        .await
        .map_err(|e| {
            warn!(error = %e, "Invalid GitHub App webhook signature");
            problem_new(StatusCode::UNAUTHORIZED)
                .with_title("Invalid Webhook Signature")
                .with_detail("The webhook signature validation failed")
        })?;

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

    let installation_id = match &webhook_event.installation {
        Some(EventInstallation::Full(installation)) => Some(installation.id.into_inner()),
        Some(EventInstallation::Minimal(installation)) => Some(installation.id.into_inner()),
        None => None,
    }
    .map(i32::try_from)
    .transpose()
    .map_err(|_| {
        problem_new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Webhook Installation")
            .with_detail("GitHub installation ID is out of range")
    })?
    .unwrap_or(0);

    let mut sender = None;
    if installation_id != 0 {
        let app = state
            .github_service
            .authorize_webhook_installation(&signing, installation_id)
            .await
            .map_err(|e| {
                warn!(installation_id, error = %e,
                    "Ignoring GitHub App webhook: installation ownership not confirmed");
                problem_new(StatusCode::UNAUTHORIZED)
                    .with_title("Webhook Installation Not Authorized")
                    .with_detail("The installation could not be bound to the signing App")
            })?;
        sender = Some(app.app_id);
    } else if !matches!(webhook_event.specific, WebhookEventPayload::Ping(_)) {
        return Err(problem_new(StatusCode::BAD_REQUEST)
            .with_title("Missing Webhook Installation")
            .with_detail("GitHub App webhook requires an installation"));
    }

    // Handle specific webhook events
    let webhook_event_clone = webhook_event.clone();
    match webhook_event.specific {
        WebhookEventPayload::Push(push_event) => {
            info!("Push event received");
            handle_push_event(&state, webhook_event_clone, push_event, installation_id).await;
        }
        WebhookEventPayload::PullRequest(_) => {
            handle_pull_request_event(&state, webhook_event_clone, &body, installation_id).await;
        }
        WebhookEventPayload::InstallationRepositories(installation_repos) => {
            info!("Installation repositories event received");
            handle_installation_repositories(&state, installation_repos, installation_id).await;
        }
        WebhookEventPayload::Installation(installation_event) => {
            info!("Installation event received");
            handle_installation_event(&state, installation_event, installation_id, sender).await;
        }
        WebhookEventPayload::Ping(ping_event) => {
            if let Some(app_id) = ping_event.hook.and_then(|h| h.app_id).filter(|id| {
                signing
                    .iter()
                    .any(|app| id.into_inner() == app.app_id as u64)
            }) {
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
    let repo = match webhook_event.repository {
        Some(repo) => repo,
        None => {
            error!("Push webhook event missing repository field, skipping");
            return;
        }
    };
    let repo_owner = match repo.owner {
        Some(owner) => owner.login,
        None => {
            error!("Push webhook event repository missing owner field, skipping");
            return;
        }
    };
    let repo_name = repo.name;

    // Check if installation exists
    let Ok(installation) = state
        .github_service
        .get_installation_by_id(installation_id)
        .await
    else {
        return;
    };

    let (branch, tag) = push_target(&push_event.r#ref, push_event.base_ref.as_deref());

    // A deleted branch retires previews only on the projects this
    // installation connects, never on a same-named repository elsewhere.
    if crate::services::git_provider_manager::GitProviderManager::is_ref_deletion(&push_event.after)
    {
        let result = match state
            .git_provider_manager
            .connection_projects(installation.connection_id, &repo_owner, &repo_name)
            .await
        {
            Ok(projects) => {
                state
                    .git_provider_manager
                    .handle_push_event_for_projects(branch, tag, push_event.after.clone(), projects)
                    .await
            }
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            error!(
                "Failed to handle branch deletion on {}/{}: {:?}",
                repo_owner, repo_name, e
            );
        }
        return;
    }

    // A project that also has its own repository webhook (a non-App
    // connection on a repository where the App is installed) receives this
    // push twice; deployment creation coalesces deliveries of the same commit.
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

/// Retire the preview of a pull request's branch once the pull request is
/// closed or merged.
async fn handle_pull_request_event(
    state: &Arc<AppState>,
    webhook_event: WebhookEvent,
    body: &[u8],
    installation_id: i32,
) {
    let Some((branch, reason)) = super::pull_request::closed_pull_request(body) else {
        return;
    };
    let Some(repo) = webhook_event.repository else {
        error!("Pull request webhook event missing repository field, skipping");
        return;
    };
    let Some(repo_owner) = repo.owner.map(|owner| owner.login) else {
        error!("Pull request webhook event repository missing owner field, skipping");
        return;
    };

    let Ok(installation) = state
        .github_service
        .get_installation_by_id(installation_id)
        .await
    else {
        return;
    };

    // Only the projects this installation connects: a same-named repository
    // on another connection is not the one whose pull request closed.
    let result = match state
        .git_provider_manager
        .connection_projects(installation.connection_id, &repo_owner, &repo.name)
        .await
    {
        Ok(projects) => {
            state
                .git_provider_manager
                .handle_branch_closed_for_projects(branch, reason, projects)
                .await
        }
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        error!(
            "Failed to queue preview teardown for {}/{} ({}): {:?}",
            repo_owner, repo.name, reason, e
        );
    }
}

/// Branch and tag a push targets, from its `ref` and (for a tag pushed from
/// a branch) its `base_ref`.
fn push_target(git_ref: &str, base_ref: Option<&str>) -> (Option<String>, Option<String>) {
    let branch = match git_ref.strip_prefix("refs/heads/") {
        Some(branch) => Some(branch.to_string()),
        None => base_ref.map(|base_ref| base_ref.replace("refs/heads/", "")),
    };
    let tag = git_ref.strip_prefix("refs/tags/").map(str::to_string);
    (branch, tag)
}

/// The parts of a GitHub repository webhook delivery Temps acts on. Every
/// repository webhook event (`ping` included) carries `repository`; only
/// `push` carries `ref` and `after`.
#[derive(Deserialize)]
struct RepoWebhookPayload {
    repository: Option<RepoWebhookRepository>,
    #[serde(rename = "ref")]
    git_ref: Option<String>,
    base_ref: Option<String>,
    after: Option<String>,
}

#[derive(Deserialize)]
struct RepoWebhookRepository {
    name: String,
    owner: RepoWebhookOwner,
}

#[derive(Deserialize)]
struct RepoWebhookOwner {
    login: String,
}

/// Receive deliveries from a per-project GitHub repository webhook.
///
/// GitHub connections authenticated with a personal access token or OAuth
/// have no GitHub App, so the App webhook never reports their pushes. Temps
/// registers a repository webhook for each such project instead, signed with
/// a secret stored encrypted on the project. A delivery is accepted only for
/// the projects whose own secret verifies `X-Hub-Signature-256` over the raw
/// body, and a push deploys only those projects.
async fn github_repo_webhook_events(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<EventResponse>, Problem> {
    let event_type = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");
    let delivery_id = headers
        .get("X-GitHub-Delivery")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    let invalid_signature = || {
        problem_new(StatusCode::UNAUTHORIZED)
            .with_title("Invalid Webhook Signature")
            .with_detail(format!(
                "GitHub repository webhook delivery {delivery_id} is not signed by any Temps project connected to this repository"
            ))
    };

    // Reject unsigned deliveries before parsing or touching the database.
    let Some(signature) = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
        .filter(|signature| signature.starts_with("sha256="))
    else {
        warn!(
            delivery_id = delivery_id,
            event_type = event_type,
            "GitHub repository webhook delivery without a sha256 signature, rejecting"
        );
        return Err(invalid_signature());
    };

    let payload: RepoWebhookPayload = serde_json::from_slice(&body).map_err(|e| {
        error!(
            delivery_id = delivery_id,
            event_type = event_type,
            "Failed to parse GitHub repository webhook payload: {}",
            e
        );
        problem_new(StatusCode::BAD_REQUEST)
            .with_title("Webhook Parse Failed")
            .with_detail(format!(
                "GitHub repository webhook delivery {delivery_id} ({event_type}) is not a valid payload: {e}"
            ))
    })?;

    let Some(repository) = payload.repository else {
        warn!(
            delivery_id = delivery_id,
            event_type = event_type,
            "GitHub repository webhook delivery names no repository, rejecting"
        );
        return Err(invalid_signature());
    };
    let owner = repository.owner.login;
    let repo = repository.name;

    let projects = state
        .git_provider_manager
        .github_repo_webhook_projects(&owner, &repo, &body, signature)
        .await
        .map_err(|e| {
            error!(
                delivery_id = delivery_id,
                "Failed to look up projects for GitHub repository webhook on {}/{}: {}",
                owner,
                repo,
                e
            );
            problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Webhook Processing Failed")
                .with_detail(format!(
                    "Failed to look up projects for GitHub repository webhook delivery {delivery_id} on {owner}/{repo}"
                ))
        })?;

    if projects.is_empty() {
        warn!(
            delivery_id = delivery_id,
            event_type = event_type,
            "No project connected to {}/{} signed this GitHub repository webhook delivery",
            owner,
            repo
        );
        return Err(invalid_signature());
    }

    let project_ids: Vec<i32> = projects.iter().map(|p| p.id).collect();

    if event_type == "pull_request" {
        let queued = match super::pull_request::closed_pull_request(&body) {
            Some((branch, reason)) => state
                .git_provider_manager
                .handle_branch_closed_for_projects(branch, reason, projects)
                .await
                .map_err(|e| {
                    error!(
                        delivery_id = delivery_id,
                        "Failed to queue preview teardown for GitHub repository webhook on {}/{} (projects {:?}): {}",
                        owner,
                        repo,
                        project_ids,
                        e
                    );
                    problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                        .with_title("Webhook Processing Failed")
                        .with_detail(format!(
                            "Failed to queue preview teardown for GitHub pull request delivery {delivery_id} on {owner}/{repo}"
                        ))
                })?,
            None => 0,
        };
        return Ok(Json(EventResponse {
            message: format!("Queued preview teardown for {queued} project(s)"),
        }));
    }

    if event_type != "push" {
        info!(
            delivery_id = delivery_id,
            event_type = event_type,
            "Verified GitHub repository webhook {} event for {}/{} (projects {:?}); nothing to deploy",
            event_type,
            owner,
            repo,
            project_ids
        );
        return Ok(Json(EventResponse {
            message: format!("{event_type} event received"),
        }));
    }

    let (branch, tag) = push_target(
        payload.git_ref.as_deref().unwrap_or_default(),
        payload.base_ref.as_deref(),
    );
    let commit = payload.after.unwrap_or_default();

    let triggered = state
        .git_provider_manager
        .handle_push_event_for_projects(branch, tag, commit, projects)
        .await
        .map_err(|e| {
            error!(
                delivery_id = delivery_id,
                "Failed to queue GitHub repository webhook push for {}/{} (projects {:?}): {}",
                owner,
                repo,
                project_ids,
                e
            );
            problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Webhook Processing Failed")
                .with_detail(format!(
                    "Failed to queue deployments for GitHub push delivery {delivery_id} on {owner}/{repo}"
                ))
        })?;

    info!(
        delivery_id = delivery_id,
        "GitHub repository webhook push for {}/{} queued for {} project(s) {:?}",
        owner,
        repo,
        triggered,
        project_ids
    );

    Ok(Json(EventResponse {
        message: format!("Processed {triggered} project(s)"),
    }))
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
    signing_app_id: Option<i32>,
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
            let payload_repos = event.repositories.clone().unwrap_or_default();

            if !payload_repos.is_empty() {
                info!(
                    "Webhook payload contains {} repositories - will store them immediately",
                    payload_repos.len()
                );
            }

            match state
                .github_service
                .process_installation(installation_id, signing_app_id)
                .await
            {
                Ok(result) => {
                    info!(
                        "Successfully processed installation {} via webhook. Result: {}",
                        installation_id, result
                    );
                    state.telemetry.report(
                        TelemetryEvent::new(TelemetryEventKind::GitProviderConnected)
                            .with("provider", "github")
                            .with("flow", "app"),
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
                    state.telemetry.report(
                        TelemetryEvent::new(TelemetryEventKind::GitProviderConnectFailed)
                            .with("provider", "github")
                            .with("flow", "app")
                            .with_failure(installation_failure_code(&e)),
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

/// Fixed failure code for a GitHub App installation that could not be
/// processed. Typed variants map directly; free-text GitHub API errors are
/// classified locally and the message itself is never sent.
fn installation_failure_code(error: &GithubAppServiceError) -> OperationFailureCode {
    match error {
        GithubAppServiceError::DatabaseError(_) => OperationFailureCode::Database,
        GithubAppServiceError::NotFound(_) | GithubAppServiceError::InstallationNotFound => {
            OperationFailureCode::NotFound
        }
        GithubAppServiceError::Conflict(_) => OperationFailureCode::Conflict,
        GithubAppServiceError::Unauthorized(_)
        | GithubAppServiceError::InvalidWebhookSignature
        | GithubAppServiceError::DecryptionFailed(_) => OperationFailureCode::Authentication,
        GithubAppServiceError::InvalidConfiguration(_)
        | GithubAppServiceError::PrivateKeyCreationFailed(_)
        | GithubAppServiceError::Validation(_) => OperationFailureCode::InvalidConfiguration,
        GithubAppServiceError::EncryptionFailed(_) | GithubAppServiceError::Other(_) => {
            OperationFailureCode::Unknown
        }
        GithubAppServiceError::GithubApiError(message) => OperationFailureCode::classify(message),
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[allow(dead_code)] // query contract of the installation callback, documented in OpenAPI
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
) -> Result<(HeaderMap, Redirect), Problem> {
    let code = params.get("code").cloned().unwrap_or_default();
    let state_param = params.get("state").cloned();
    let installation_id = params
        .get("installation_id")
        .and_then(|id| id.parse::<i64>().ok());
    let setup_action = params.get("setup_action").cloned();

    // Log parameter presence only — `code` is a sensitive OAuth authorization code.
    info!(
        "GitHub App auth callback - param keys: {:?}, installation_id={:?}, setup_action={:?}",
        params.keys().collect::<Vec<_>>(),
        installation_id,
        setup_action
    );

    if code.is_empty() {
        return Err(problem_new(StatusCode::BAD_REQUEST)
            .with_title("Missing Authorization Code")
            .with_detail("The 'code' parameter is required"));
    }

    // Use the operator-configured external URL — never reflect client headers.
    let external_url = state
        .config_service
        .get_external_url()
        .await
        .map_err(|e| {
            problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Settings Unavailable")
                .with_detail(format!("Failed to load external URL: {}", e))
        })?
        .ok_or_else(|| {
            problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("External URL Not Configured")
                .with_detail(
                    "Server external_url must be configured before completing GitHub App installation",
                )
        })?;
    let external_url = external_url.trim_end_matches('/').to_string();

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
                "{}/git-providers/{}?installation_id={}&github_installation_processing=true",
                external_url, pid, installation_id
            )
        } else {
            format!(
                "{}/git-sources?installation_id={}&github_installation_processing=true",
                external_url, installation_id
            )
        };

        let mut response_headers = HeaderMap::new();
        response_headers.insert(
            "Cache-Control",
            axum::http::HeaderValue::from_static("no-store"),
        );

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
        return handle_manifest_conversion_with_source(&state, code, state_param).await;
    }

    // For auth-only flow (no installation_id yet), redirect and wait for webhook
    info!("OAuth authorization code received without installation_id - waiting for installation webhook");

    let redirect_url = format!("{}/dashboard", external_url);

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        "Cache-Control",
        axum::http::HeaderValue::from_static("no-store"),
    );

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
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<(HeaderMap, Redirect), Problem> {
    // Installation callback may or may not have a code
    let code = params.get("code").cloned();
    let installation_id = params
        .get("installation_id")
        .and_then(|id| id.parse::<i64>().ok());
    let setup_action = params.get("setup_action").cloned();

    // Log the received parameters for debugging
    info!(
        "GitHub App callback params - code: {:?}, installation_id: {:?}, setup_action: {:?}",
        code.as_ref().map(|_| "present"),
        installation_id,
        setup_action
    );

    // Installation callback must have installation_id
    let installation_id = match installation_id {
        Some(id) => id,
        None => {
            return Err(problem_new(StatusCode::BAD_REQUEST)
                .with_title("Missing Installation ID")
                .with_detail(
                    "The 'installation_id' parameter is required for installation callback",
                ));
        }
    };

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

    // Use the operator-configured external URL — never reflect client headers.
    let external_url = state
        .config_service
        .get_external_url()
        .await
        .map_err(|e| {
            problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Settings Unavailable")
                .with_detail(format!("Failed to load external URL: {}", e))
        })?
        .ok_or_else(|| {
            problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("External URL Not Configured")
                .with_detail(
                    "Server external_url must be configured before completing GitHub App installation",
                )
        })?;
    let external_url = external_url.trim_end_matches('/');

    // Redirect to dashboard - let the webhook handle installation creation
    let redirect_url = format!("{}/dashboard", external_url);

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        "Cache-Control",
        axum::http::HeaderValue::from_static("no-store"),
    );

    Ok((response_headers, Redirect::to(&redirect_url)))
}

#[cfg(test)]
mod tests {
    /// Deliver a push signed by App 10, whose secret App 20 shares when
    /// `shared` is set, for an installation owned by `provider_id`.
    async fn signed_installation_delivery(provider_id: i32, shared: bool) -> (bool, usize) {
        use hmac::{Hmac, KeyInit, Mac};
        use sea_orm::{DatabaseBackend, MockDatabase};
        use temps_entities::{git_provider_connections, git_providers, projects};
        let now = chrono::Utc::now();
        let app = git_providers::Model {
            id: 10,
            name: "example-app".into(),
            provider_type: "github".into(),
            base_url: None,
            api_url: None,
            auth_method: "app".into(),
            auth_config: serde_json::json!({
                "app_id": 100, "client_id": "example", "client_secret": "example",
                "private_key": "unused", "webhook_secret": "test-signing-secret"
            }),
            webhook_secret: None,
            is_active: true,
            is_default: false,
            created_at: now,
            updated_at: now,
        };
        let mut other_app = app.clone();
        other_app.id = 20;
        other_app.auth_config["app_id"] = serde_json::json!(101);
        if !shared {
            other_app.auth_config["webhook_secret"] = serde_json::json!("other-test-secret");
        }
        let connection: git_provider_connections::Model =
            serde_json::from_value(serde_json::json!({
                "id": 30, "provider_id": provider_id, "account_name": "example",
                "account_type": "Organization", "installation_id": "200",
                "is_active": true, "is_expired": false, "syncing": false,
                "synced_repository_count": 0, "health_status": "unknown",
                "consecutive_health_failures": 0, "created_at": now, "updated_at": now
            }))
            .unwrap();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![other_app, app]])
                .append_query_results([vec![connection.clone()], vec![connection]])
                .append_query_results([Vec::<projects::Model>::new()])
                .into_connection(),
        );
        let state = super::super::public_route_tests::state_with_db(db.clone());
        let body = serde_json::to_vec(&serde_json::json!({
            "installation": {"id": 200, "node_id": "example-installation"},
            "repository": {"id": 1, "name": "demo", "owner": {
                "login": "example", "id": 1, "node_id": "example",
                "avatar_url": "https://example.com/avatar", "gravatar_id": "",
                "url": "https://example.com/user", "html_url": "https://example.com/user",
                "followers_url": "https://example.com/followers",
                "following_url": "https://example.com/following",
                "gists_url": "https://example.com/gists", "starred_url": "https://example.com/starred",
                "subscriptions_url": "https://example.com/subscriptions",
                "organizations_url": "https://example.com/orgs",
                "repos_url": "https://example.com/repos", "events_url": "https://example.com/events",
                "received_events_url": "https://example.com/received", "type": "Organization",
                "site_admin": false
            }, "url": "https://example.com/repo"},
            "ref": "refs/heads/topic", "after": "0000000000000000000000000000000000000000",
            "before": "1111111111111111111111111111111111111111",
            "commits": [], "compare": "https://example.com/compare",
            "created": false, "deleted": true, "forced": false,
            "pusher": {"name": "example", "email": "example@example.com"}
        })).unwrap();
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(b"test-signing-secret").unwrap();
        mac.update(&body);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        let mut headers = HeaderMap::new();
        headers.insert("X-Hub-Signature-256", signature.parse().unwrap());
        headers.insert("X-GitHub-Event", "push".parse().unwrap());
        let result = github_webhook_events(headers, State(state.clone()), Bytes::from(body)).await;
        let owned = provider_id == 10 || (shared && provider_id == 20);
        if !owned {
            assert_eq!(
                result.as_ref().err().unwrap().status_code,
                StatusCode::UNAUTHORIZED
            );
        } else {
            assert!(result.is_ok(), "{:?}", result.as_ref().err());
        }
        drop(state);
        let queries = Arc::try_unwrap(db).unwrap().into_transaction_log().len();
        (result.is_ok(), queries)
    }

    #[tokio::test]
    async fn signing_app_mismatch_dispatches_nothing() {
        // Only signature and ownership queries run; no dispatch queries or jobs.
        assert_eq!(signed_installation_delivery(20, false).await, (false, 2));
    }

    #[tokio::test]
    async fn shared_secret_binds_the_app_that_owns_the_installation() {
        // App 20 is checked first and shares the secret, but App 10 owns it.
        assert_eq!(signed_installation_delivery(10, true).await, (true, 4));
    }

    #[tokio::test]
    async fn shared_secret_still_rejects_an_installation_no_signer_owns() {
        assert_eq!(signed_installation_delivery(30, true).await, (false, 2));
    }

    #[tokio::test]
    async fn signing_app_pairing_dispatches_branch_deletion() {
        // The matching App reaches the installation and project dispatch queries.
        assert_eq!(signed_installation_delivery(10, false).await, (true, 4));
    }

    use super::*;

    #[test]
    fn installation_failures_map_to_fixed_codes() {
        assert_eq!(
            installation_failure_code(&GithubAppServiceError::InstallationNotFound),
            OperationFailureCode::NotFound
        );
        assert_eq!(
            installation_failure_code(&GithubAppServiceError::Unauthorized(
                "bad credentials".to_string()
            )),
            OperationFailureCode::Authentication
        );
        assert_eq!(
            installation_failure_code(&GithubAppServiceError::GithubApiError(
                "operation timed out".to_string()
            )),
            OperationFailureCode::Timeout
        );
    }

    #[test]
    fn push_target_reads_branch_and_tag_refs() {
        assert_eq!(
            push_target("refs/heads/main", None),
            (Some("main".to_string()), None)
        );
        assert_eq!(
            push_target("refs/heads/feature/a-b", None),
            (Some("feature/a-b".to_string()), None)
        );
        assert_eq!(
            push_target("refs/tags/v1.2.0", Some("refs/heads/main")),
            (Some("main".to_string()), Some("v1.2.0".to_string()))
        );
        assert_eq!(
            push_target("refs/tags/v1.2.0", None),
            (None, Some("v1.2.0".to_string()))
        );
        assert_eq!(push_target("", None), (None, None));
    }

    fn detail_of(problem: &Problem) -> String {
        problem
            .body
            .get("detail")
            .and_then(|detail| detail.as_str())
            .unwrap_or_default()
            .to_string()
    }

    #[tokio::test]
    async fn manifest_conversion_returns_the_app_github_created() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/app-manifests/abc123/conversions")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":42,"client_id":"client"}"#)
            .create_async()
            .await;

        let app = fetch_manifest_app(&server.url(), "abc123")
            .await
            .expect("a 201 reply is a created app");

        mock.assert_async().await;
        assert_eq!(app["id"], 42);
    }

    #[tokio::test]
    async fn manifest_conversion_rejects_a_status_that_is_not_success() {
        for status in [404, 422, 500] {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", "/app-manifests/not-a-code/conversions")
                .with_status(status)
                .with_header("content-type", "application/json")
                .with_body(r#"{"message":"Not Found"}"#)
                .create_async()
                .await;

            let problem = fetch_manifest_app(&server.url(), "not-a-code")
                .await
                .expect_err("an error status must not yield an app");

            mock.assert_async().await;
            assert_eq!(problem.status_code, StatusCode::BAD_GATEWAY);
            assert!(
                detail_of(&problem).contains(&status.to_string()),
                "detail should name GitHub's status {status}: {}",
                detail_of(&problem)
            );
        }
    }

    #[tokio::test]
    async fn manifest_code_is_percent_encoded_into_the_request_path() {
        let mut server = mockito::Server::new_async().await;
        // Without encoding, `/`, `?` and `#` would change which URL is requested.
        let mock = server
            .mock("POST", "/app-manifests/a%2Fb%3Fc%23d/conversions")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":1}"#)
            .create_async()
            .await;

        fetch_manifest_app(&server.url(), "a/b?c#d")
            .await
            .expect("the encoded path should reach the mock");

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_conversion_errors_do_not_contain_the_code() {
        // Nothing listens on port 1, so the request fails before any reply.
        let problem = fetch_manifest_app("http://127.0.0.1:1", "one-time-code-value")
            .await
            .expect_err("the connection is refused");

        assert_eq!(problem.status_code, StatusCode::BAD_GATEWAY);
        assert!(!detail_of(&problem).contains("one-time-code-value"));
    }

    #[test]
    fn an_unusable_github_reply_is_a_502_that_names_the_missing_fields() {
        let problem = app_creation_problem(GithubAppServiceError::Validation(
            "GitHub's reply is missing a usable value for: pem".to_string(),
        ));

        assert_eq!(problem.status_code, StatusCode::BAD_GATEWAY);
        assert!(detail_of(&problem).contains("pem"));
    }

    #[test]
    fn an_app_that_is_already_registered_is_a_409() {
        let problem = app_creation_problem(GithubAppServiceError::Conflict(
            "GitHub App with id 7 already exists as provider 3".to_string(),
        ));

        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        assert!(detail_of(&problem).contains("provider 3"));
    }

    #[test]
    fn every_other_failure_stays_a_500() {
        for error in [
            GithubAppServiceError::DatabaseError(sea_orm::DbErr::Custom("db".to_string())),
            GithubAppServiceError::GithubApiError("api".to_string()),
            GithubAppServiceError::NotFound("app".to_string()),
            GithubAppServiceError::DecryptionFailed("key".to_string()),
            GithubAppServiceError::EncryptionFailed("key".to_string()),
            GithubAppServiceError::PrivateKeyCreationFailed("key".to_string()),
            GithubAppServiceError::InstallationNotFound,
            GithubAppServiceError::InvalidWebhookSignature,
            GithubAppServiceError::Unauthorized("no".to_string()),
            GithubAppServiceError::InvalidConfiguration("cfg".to_string()),
            GithubAppServiceError::Other("other".to_string()),
        ] {
            let description = format!("{error:?}");
            let problem = app_creation_problem(error);
            assert_eq!(
                problem.status_code,
                StatusCode::INTERNAL_SERVER_ERROR,
                "{description}"
            );
        }
    }
}
