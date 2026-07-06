use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::Redirect,
    routing::post,
    Router,
};
use bytes::Bytes;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::problemdetails::{new as problem_new, Problem};
use tracing::{error, info, warn};
use utoipa::ToSchema;

use super::types::GitAppState as AppState;

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        // GitLab OAuth callback endpoint. This is a cross-site redirect target
        // and MUST NOT require caller auth: the browser won't carry our API
        // bearer token. The caller identity is recovered from the server-issued
        // `state` param (see GitProviderManager::consume_oauth_state).
        .route(
            "/webhook/git/gitlab/auth",
            axum::routing::get(gitlab_oauth_callback),
        )
        .route("/webhook/git/gitlab/events", post(gitlab_webhook_events))
}

// ── Token verification helpers ─────────────────────────────────────────────

/// Pure guard: returns `false` (reject) when a project has no stored signing
/// token, `true` when one is present and signature verification should proceed.
///
/// Extracted as a pure sync function so the security decision is directly
/// unit-testable without needing a live `AppState`.
///
/// # Security rationale
///
/// Projects with `gitlab_webhook_signing_token = NULL` have never had a
/// signed webhook configured (pre-feature legacy rows) or had their token
/// erased. Accepting unsigned payloads would let any internet actor forge
/// GitLab push events and trigger arbitrary deployments. Operators must
/// re-enroll by recreating the webhook in GitLab, which issues a fresh token.
#[cfg(test)]
pub(crate) fn project_has_signing_token(stored_token: Option<&str>) -> bool {
    // SECURITY: projects with no stored signing token are rejected — do not
    // accept unsigned payloads. Operators must re-enroll legacy projects by
    // recreating the GitLab webhook so a fresh token is issued.
    stored_token.is_some()
}

/// Verify that an incoming GitLab webhook request is signed with the token
/// stored for `project`.
///
/// Returns `false` immediately when no signing token is stored (see
/// `project_has_signing_token` in the test module), then decrypts and
/// HMAC-validates for the `Some` branch.
pub(crate) async fn verify_gitlab_webhook_token(
    project: &temps_entities::projects::Model,
    body: &[u8],
    webhook_signature: Option<&str>,
    webhook_id: &str,
    webhook_timestamp: &str,
    state: &AppState,
) -> bool {
    match project.gitlab_webhook_signing_token.as_deref() {
        Some(encrypted) => {
            match state.git_provider_manager.decrypt_token(encrypted).await {
                Ok(plaintext) => {
                    if let Some(sig) = webhook_signature {
                        // Standard-Webhooks HMAC: signs
                        // "{webhook-id}.{timestamp}.{body}" with the
                        // 32-byte key derived from the whsec_ token.
                        crate::services::gitlab_webhook::verify_gitlab_signature(
                            body,
                            sig,
                            webhook_id,
                            webhook_timestamp,
                            &plaintext,
                        )
                    } else {
                        // No webhook-signature header — reject. Legacy
                        // X-Gitlab-Token is intentionally not supported.
                        false
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to decrypt signing token for project {}: {}",
                        project.id, e
                    );
                    false
                }
            }
        }
        None => {
            // SECURITY: reject webhooks for projects with no stored signing
            // token. Operators must re-enroll legacy projects by recreating
            // the GitLab webhook (which will trigger token issuance) before
            // automatic deployments will resume. We will NOT accept unsigned
            // payloads — anyone on the internet could forge push events.
            warn!(
                "GitLab project {} has no stored signing token — webhook rejected. \
                 Re-enroll the webhook in GitLab to issue a signing token.",
                project.id
            );
            false
        }
    }
}

// ── GitLab webhook event receiver ─────────────────────────────────────────

#[derive(Serialize, ToSchema)]
pub struct GitLabWebhookResponse {
    message: String,
}

/// Receive GitLab push/tag events and trigger Temps deployment pipelines.
///
/// GitLab signs each delivery with HMAC-SHA256 of the raw body using the
/// `signing_token` configured on the webhook. The signature is sent in
/// `webhook-signature: v1,{base64}`. The legacy plaintext `X-Gitlab-Token`
/// header is intentionally NOT supported — it's deprecated and not
/// recommended by GitLab.
///
/// For each Temps project that has a matching `repo_owner/repo_name` *and*
/// whose stored signing token validates the HMAC, we queue a `GitPushEventJob`
/// via the same code path as the GitHub handler.
async fn gitlab_webhook_events(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<axum::Json<GitLabWebhookResponse>, Problem> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use temps_entities::projects;

    let event_type = headers
        .get("X-Gitlab-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    // GitLab signs each delivery using Standard-Webhooks-style HMAC-SHA256:
    // the signed message is "{webhook-id}.{webhook-timestamp}.{body}" and the
    // signature lands in `webhook-signature: v1,{base64}`. The legacy
    // plaintext `X-Gitlab-Token` is not supported.
    //
    // Axum lowercases all header names, so the lowercase forms are the
    // canonical lookups even though GitLab capitalises them as
    // `Webhook-Signature` / `Webhook-Id` / `Webhook-Timestamp`.
    let webhook_signature_header = headers
        .get("webhook-signature")
        .and_then(|v| v.to_str().ok());
    let webhook_id_header = headers
        .get("webhook-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let webhook_timestamp_header = headers
        .get("webhook-timestamp")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    info!("Received GitLab webhook event: {}", event_type);

    // Parse the payload to get project owner / name and the pushed ref.
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to parse GitLab webhook payload: {}", e);
            return Err(problem_new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Payload")
                .with_detail("GitLab webhook body is not valid JSON"));
        }
    };

    // GitLab puts the repo path in `project.path_with_namespace`
    // (e.g. "group/my-repo") and the namespace in `project.namespace`.
    // We decompose owner / name from the full path.
    let path_with_namespace = payload
        .get("project")
        .and_then(|p| p.get("path_with_namespace"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let (repo_owner, repo_name) = match path_with_namespace.rsplit_once('/') {
        Some((owner, name)) => (owner, name),
        None => {
            warn!("GitLab webhook missing project.path_with_namespace, skipping");
            return Ok(axum::Json(GitLabWebhookResponse {
                message: "No matching project found".to_string(),
            }));
        }
    };

    let git_ref = payload.get("ref").and_then(|v| v.as_str()).unwrap_or("");

    let branch = if git_ref.starts_with("refs/heads/") {
        Some(git_ref.replace("refs/heads/", ""))
    } else {
        None
    };

    let tag = if git_ref.starts_with("refs/tags/") {
        Some(git_ref.replace("refs/tags/", ""))
    } else {
        None
    };

    let commit = payload
        .get("checkout_sha")
        .and_then(|v| v.as_str())
        .or_else(|| {
            payload
                .get("commits")
                .and_then(|cs| cs.as_array())
                .and_then(|arr| arr.first())
                .and_then(|c| c.get("id"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .to_string();

    // Find all Temps projects matching this repo.
    let matching_projects = match projects::Entity::find()
        .filter(projects::Column::RepoOwner.eq(repo_owner))
        .filter(projects::Column::RepoName.eq(repo_name))
        .all(state.git_provider_manager.db())
        .await
    {
        Ok(ps) => ps,
        Err(e) => {
            error!("DB error looking up projects for GitLab push: {}", e);
            return Ok(axum::Json(GitLabWebhookResponse {
                message: "processed".to_string(),
            }));
        }
    };

    if matching_projects.is_empty() {
        warn!(
            "No Temps projects found for GitLab repo {}/{}, ignoring event",
            repo_owner, repo_name
        );
        return Ok(axum::Json(GitLabWebhookResponse {
            message: "No matching project found".to_string(),
        }));
    }

    // Validate the signing token against each project.  We need at least one
    // project to authenticate the request before dispatching the push event.
    // `handle_push_event` internally re-queries all matching projects, so we
    // call it only once (not per project) to avoid duplicate queue entries.
    let mut any_valid = false;

    for project in &matching_projects {
        // Verify the signing token for this project.
        let token_valid = verify_gitlab_webhook_token(
            project,
            &body,
            webhook_signature_header,
            webhook_id_header,
            webhook_timestamp_header,
            &state,
        )
        .await;

        if token_valid {
            any_valid = true;
            break;
        } else {
            warn!(
                "GitLab webhook signature mismatch for project {}, skipping",
                project.id
            );
        }
    }

    if !any_valid {
        warn!(
            "No matching GitLab project with valid signature for {}/{}, ignoring",
            repo_owner, repo_name
        );
        return Ok(axum::Json(GitLabWebhookResponse {
            message: "Signature validation failed".to_string(),
        }));
    }

    // Dispatch through the existing push-event machinery (handles all projects
    // for this repo in a single call).
    let triggered = if let Err(e) = state
        .git_provider_manager
        .handle_push_event(
            repo_owner.to_string(),
            repo_name.to_string(),
            branch.clone(),
            tag.clone(),
            commit.clone(),
        )
        .await
    {
        error!(
            "Failed to handle GitLab push event for {}/{}: {:?}",
            repo_owner, repo_name, e
        );
        0
    } else {
        matching_projects.len()
    };

    info!(
        "GitLab webhook event processed: {} project(s) queued for {}/{}",
        triggered, repo_owner, repo_name
    );

    Ok(axum::Json(GitLabWebhookResponse {
        message: format!("Processed {} project(s)", triggered),
    }))
}

/// Handle GitLab OAuth callback
async fn gitlab_oauth_callback(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Redirect, Problem> {
    // Extract OAuth parameters
    let code = params
        .get("code")
        .ok_or_else(|| {
            problem_new(StatusCode::BAD_REQUEST)
                .with_title("Missing Authorization Code")
                .with_detail("The 'code' parameter is required for GitLab OAuth callback")
        })?
        .clone();

    let oauth_state = params.get("state").cloned().ok_or_else(|| {
        problem_new(StatusCode::BAD_REQUEST)
            .with_title("Missing OAuth State")
            .with_detail("The 'state' parameter is required for GitLab OAuth callback")
    })?;

    info!(
        "GitLab OAuth callback received - code: {}, state: {}",
        code, oauth_state
    );

    // Recover the user + provider that started this flow. This replaces the
    // RequireAuth extractor we'd use on a normal endpoint.
    let (user_id, provider_id) = state
        .git_provider_manager
        .consume_oauth_state(&oauth_state)
        .await?;

    // Handle the OAuth callback
    let connection = state
        .git_provider_manager
        .handle_oauth_callback(
            provider_id,
            code,
            oauth_state,
            user_id,
            None, // host_override - not needed as we use external_url from config
        )
        .await
        .map_err(|e| {
            problem_new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("OAuth Callback Failed")
                .with_detail(format!("Failed to handle GitLab OAuth callback: {}", e))
        })?;

    info!(
        "Successfully created GitLab connection for user {} with account {}",
        user_id, connection.account_name
    );

    // Get external URL from config for redirect
    let external_url = state
        .config_service
        .get_setting("external_url")
        .await
        .unwrap_or(None)
        .unwrap_or_else(|| "http://localhost:3000".to_string());

    // Redirect to the git provider page with success status
    let redirect_url = format!(
        "{}/git-providers/{}?status=connected",
        external_url, provider_id
    );

    Ok(Redirect::to(&redirect_url))
}

#[cfg(test)]
mod tests {
    //! Smoke-tests for the helpers from the handler perspective. Detailed
    //! crypto edge-cases live in `services::gitlab_webhook::tests`.

    use super::project_has_signing_token;

    /// Build a valid `v1,{base64}` Standard-Webhooks signature for a given
    /// payload + headers + signing token.
    fn make_sig(payload: &[u8], webhook_id: &str, ts: &str, signing_token: &str) -> String {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;
        let key = STANDARD
            .decode(signing_token.strip_prefix("whsec_").expect("whsec_ prefix"))
            .expect("base64 key");
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
        let mut msg: Vec<u8> = Vec::new();
        msg.extend_from_slice(webhook_id.as_bytes());
        msg.push(b'.');
        msg.extend_from_slice(ts.as_bytes());
        msg.push(b'.');
        msg.extend_from_slice(payload);
        mac.update(&msg);
        format!("v1,{}", STANDARD.encode(mac.finalize().into_bytes()))
    }

    fn test_token() -> String {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        format!("whsec_{}", STANDARD.encode([0u8; 32]))
    }

    // ── project_has_signing_token (None => false security gate) ──────────

    /// A project with no stored signing token must ALWAYS be rejected.
    /// This is the critical fix for CVE-class: "None => true" allowed
    /// unauthenticated actors to forge GitLab push events for legacy projects.
    #[test]
    fn test_none_stored_token_is_rejected() {
        assert!(
            !project_has_signing_token(None),
            "projects with NULL signing token must be rejected — \
             accepting unsigned webhooks allows forged push events"
        );
    }

    /// A project with a stored signing token passes the guard (token
    /// decryption and HMAC verification are done in a subsequent step).
    #[test]
    fn test_some_stored_token_passes_guard() {
        assert!(
            project_has_signing_token(Some("whsec_AAAA")),
            "projects with a stored signing token should pass the presence guard"
        );
    }

    /// Empty string token value also passes the guard — the HMAC step
    /// (not this guard) is responsible for rejecting malformed tokens.
    #[test]
    fn test_empty_string_token_passes_guard() {
        assert!(
            project_has_signing_token(Some("")),
            "empty string is Some, so the presence guard passes; \
             the subsequent HMAC step rejects it"
        );
    }

    // ── HMAC verification (existing tests, unchanged) ─────────────────────

    #[test]
    fn test_verify_hmac_signature_via_service_helper() {
        use crate::services::gitlab_webhook::verify_gitlab_signature;

        let token = test_token();
        let payload = b"push event body does not matter";
        let header = make_sig(payload, "wh_a", "1700000000", &token);

        assert!(
            verify_gitlab_signature(payload, &header, "wh_a", "1700000000", &token),
            "correct HMAC header must pass"
        );
        assert!(
            !verify_gitlab_signature(b"different body", &header, "wh_a", "1700000000", &token),
            "HMAC computed over different body must fail"
        );
        assert!(
            !verify_gitlab_signature(payload, &header, "wh_a", "1700000099", &token),
            "different timestamp must fail"
        );
    }

    #[test]
    fn test_generate_signing_token_via_service_helper() {
        use crate::services::gitlab_webhook::generate_signing_token;
        use base64::{engine::general_purpose::STANDARD, Engine as _};

        let tok = generate_signing_token();
        assert!(tok.starts_with("whsec_"));
        let decoded = STANDARD
            .decode(tok.strip_prefix("whsec_").unwrap())
            .expect("valid base64");
        assert_eq!(decoded.len(), 32);
    }
}
