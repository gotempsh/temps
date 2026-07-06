use axum::extract::DefaultBodyLimit;
use axum::{extract::State, http::HeaderMap, routing::post, Router};
use bytes::Bytes;
use serde::Serialize;
use std::sync::Arc;
use tracing::{error, info, warn};
use utoipa::ToSchema;

use super::types::GitAppState as AppState;

/// Register Gitea webhook routes.
///
/// Applies `DefaultBodyLimit::max(512 KiB)` to the webhook endpoint
/// (MUST-FIX 2 from the security review). Gitea webhook deliveries are
/// typically a few KiB; the 512 KiB limit provides a large safety margin
/// while preventing memory exhaustion from oversized payloads.
pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/webhook/git/gitea/events",
        post(gitea_webhook_events).layer(DefaultBodyLimit::max(512 * 1024)),
    )
}

// ── Token helpers ─────────────────────────────────────────────────────────────

/// Returns `true` when the project has a stored Gitea signing token that can
/// be used for HMAC verification.
///
/// Projects with `gitea_webhook_signing_token = NULL` have no signed webhook
/// configured. Accepting unsigned payloads would allow any actor on the
/// internet to forge push events and trigger deployments.
#[cfg(test)]
pub(crate) fn project_has_signing_token(stored_token: Option<&str>) -> bool {
    stored_token.is_some()
}

/// Verify the `X-Gitea-Signature` header against the raw request body and
/// the project's stored signing token.
///
/// Decrypts the stored token, then delegates HMAC-SHA256 verification to
/// `GiteaProvider::verify_webhook_signature`.
///
/// # Security
/// - HMAC is verified on the **raw bytes** before JSON parsing.
/// - Returns `false` rather than propagating errors (caller logs and returns HTTP 200).
/// - The signing token is never logged.
pub(crate) async fn verify_gitea_webhook_signature(
    project: &temps_entities::projects::Model,
    body: &[u8],
    signature: Option<&str>,
    state: &AppState,
) -> bool {
    let encrypted = match project.gitea_webhook_signing_token.as_deref() {
        Some(enc) => enc,
        None => {
            // SECURITY: projects with no stored signing token are rejected.
            // Operators must connect a Gitea webhook (which issues a fresh
            // signing token) before automatic deployments will resume.
            warn!(
                "Gitea project {} has no stored signing token — webhook rejected. \
                 Re-enroll the webhook to issue a signing token.",
                project.id
            );
            return false;
        }
    };

    let plaintext = match state.git_provider_manager.decrypt_token(encrypted).await {
        Ok(t) => t,
        Err(e) => {
            warn!(
                "Failed to decrypt Gitea signing token for project {}: {}",
                project.id, e
            );
            return false;
        }
    };

    let sig = match signature {
        Some(s) => s,
        None => {
            warn!(
                "Gitea webhook for project {} missing X-Gitea-Signature header",
                project.id
            );
            return false;
        }
    };

    // Verify HMAC-SHA256 on raw bytes (before JSON parsing).
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    let decoded = match hex::decode(sig) {
        Ok(d) => d,
        Err(_) => return false,
    };

    let mut mac = match Hmac::<Sha256>::new_from_slice(plaintext.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    mac.verify_slice(&decoded).is_ok()
}

// ── Webhook handler ───────────────────────────────────────────────────────────

#[derive(Serialize, ToSchema)]
pub struct GiteaWebhookResponse {
    message: String,
}

/// Receive Gitea push events and trigger Temps deployment pipelines.
///
/// Gitea signs each delivery with `HMAC-SHA256(key=secret, msg=raw_body)`,
/// transmitting the hex digest in `X-Gitea-Signature`.
///
/// Verification uses the raw body bytes BEFORE JSON parsing (MUST-FIX from
/// the security review).
///
/// # Security
/// - Body is limited to 512 KiB via `DefaultBodyLimit` at route registration
///   (MUST-FIX 2).
/// - Signature is verified on raw bytes before JSON parsing.
/// - Returns **HTTP 200** on signature failure (matching GitLab, not GitHub's
///   401) to avoid leaking whether the signature check ran (MUST-FIX from
///   Section 4 of the ADR).
/// - `X-Gitea-Delivery` is logged for traceability; the signing token is
///   never logged.
async fn gitea_webhook_events(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> axum::Json<GiteaWebhookResponse> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use temps_entities::projects;

    let event_type = headers
        .get("X-Gitea-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    // Log the delivery ID for traceability, never the signing token or path.
    let delivery_id = headers
        .get("X-Gitea-Delivery")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    // X-Gitea-Signature: hex(HMAC-SHA256(key=secret, msg=body))
    let signature = headers
        .get("X-Gitea-Signature")
        .and_then(|v| v.to_str().ok());

    info!(
        delivery_id = delivery_id,
        event_type = event_type,
        "Received Gitea webhook event"
    );

    // SECURITY: Parse JSON minimally to locate repo owner/name so we can
    // look up matching projects, but do NOT branch on event_type yet —
    // we must verify the HMAC FIRST to prevent an event-type oracle where
    // an unsigned sender can distinguish "not a push" from "bad signature".
    // (MAJOR security finding: reordered so HMAC verification precedes any
    // event-type branching. All failure paths return HTTP 200 uniformly.)
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            error!(
                delivery_id = delivery_id,
                "Failed to parse Gitea webhook payload: {}", e
            );
            // Return 200 even for malformed JSON (ADR Decision 4).
            return axum::Json(GiteaWebhookResponse {
                message: "processed".to_string(),
            });
        }
    };

    // Extract repo owner and name from the Gitea push payload.
    // Gitea puts these in `repository.owner.login` and `repository.name`.
    // These fields are present in all event types so we can use them for
    // project lookup before we know whether the event is a push.
    let repo_owner = payload
        .get("repository")
        .and_then(|r| r.get("owner"))
        .and_then(|o| o.get("login"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let repo_name = payload
        .get("repository")
        .and_then(|r| r.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if repo_owner.is_empty() || repo_name.is_empty() {
        warn!(
            delivery_id = delivery_id,
            "Gitea webhook missing repository owner/name, skipping"
        );
        return axum::Json(GiteaWebhookResponse {
            message: "processed".to_string(),
        });
    }

    // Look up Temps projects matching this repository so we have signing
    // tokens to verify against. HMAC verification happens next, before
    // any event-type branching.
    let matching_projects = match projects::Entity::find()
        .filter(projects::Column::RepoOwner.eq(repo_owner))
        .filter(projects::Column::RepoName.eq(repo_name))
        .all(state.git_provider_manager.db())
        .await
    {
        Ok(ps) => ps,
        Err(e) => {
            error!(
                delivery_id = delivery_id,
                "DB error looking up projects for Gitea webhook: {}", e
            );
            // Return 200 even on DB errors to avoid leaking internal state.
            return axum::Json(GiteaWebhookResponse {
                message: "processed".to_string(),
            });
        }
    };

    if matching_projects.is_empty() {
        // No projects matched — return 200 without any distinguishable message
        // so an attacker cannot determine whether the repo exists in Temps.
        return axum::Json(GiteaWebhookResponse {
            message: "processed".to_string(),
        });
    }

    // SECURITY (MAJOR fix): Verify the HMAC signature BEFORE branching on
    // event type. This prevents a non-push event from returning a different
    // response ("event type not handled") that an unsigned sender could use
    // as an oracle to distinguish event types from signature failures.
    let mut any_valid = false;

    for project in &matching_projects {
        if verify_gitea_webhook_signature(project, &body, signature, &state).await {
            any_valid = true;
            break;
        } else {
            warn!(
                delivery_id = delivery_id,
                project_id = project.id,
                "Gitea webhook signature mismatch for project"
            );
        }
    }

    if !any_valid {
        // SECURITY: Return HTTP 200 (not 401/403) so the caller cannot
        // determine whether a signature check was performed or whether the
        // project exists (ADR Decision 4, matching gitlab.rs:285).
        warn!(
            delivery_id = delivery_id,
            repo_owner = repo_owner,
            repo_name = repo_name,
            "No matching Gitea project with valid signature — ignoring"
        );
        return axum::Json(GiteaWebhookResponse {
            message: "processed".to_string(),
        });
    }

    // Signature is valid. NOW branch on event type.
    // Non-push events are accepted (200) and logged but not dispatched.
    if event_type != "push" {
        info!(
            delivery_id = delivery_id,
            event_type = event_type,
            "Ignoring non-push Gitea event (signature verified)"
        );
        return axum::Json(GiteaWebhookResponse {
            message: "processed".to_string(),
        });
    }

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
        .get("after")
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

    // Dispatch through the existing push-event machinery.
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
            delivery_id = delivery_id,
            repo_owner = repo_owner,
            repo_name = repo_name,
            "Failed to handle Gitea push event: {:?}",
            e
        );
        0
    } else {
        matching_projects.len()
    };

    info!(
        delivery_id = delivery_id,
        triggered = triggered,
        repo_owner = repo_owner,
        repo_name = repo_name,
        "Gitea webhook event processed"
    );

    axum::Json(GiteaWebhookResponse {
        message: format!("Processed {} project(s)", triggered),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── project_has_signing_token ─────────────────────────────────────────────

    #[test]
    fn test_none_token_is_rejected() {
        assert!(
            !project_has_signing_token(None),
            "projects with NULL signing token must be rejected"
        );
    }

    #[test]
    fn test_some_token_passes_guard() {
        assert!(
            project_has_signing_token(Some("deadbeef")),
            "projects with a stored signing token should pass the presence guard"
        );
    }

    #[test]
    fn test_empty_string_passes_guard() {
        // Empty string is Some(...); HMAC verification rejects it in the next step.
        assert!(project_has_signing_token(Some("")));
    }

    // ── HMAC verification (via GiteaProvider directly) ────────────────────────

    #[tokio::test]
    async fn test_gitea_hmac_via_provider() {
        use crate::services::git_provider::{AuthMethod, GitProviderService};
        use crate::services::gitea_provider::GiteaProvider;
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;

        let secret = "test-secret";
        let payload = b"gitea push event body";

        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload);
        let sig = hex::encode(mac.finalize().into_bytes());

        let provider = GiteaProvider::new(
            "https://git.example.com".to_string(),
            AuthMethod::PersonalAccessToken {
                token: "tok".to_string(),
            },
        );

        assert!(provider
            .verify_webhook_signature(payload, &sig, secret)
            .await
            .unwrap());
        assert!(!provider
            .verify_webhook_signature(b"different", &sig, secret)
            .await
            .unwrap());
    }
}
