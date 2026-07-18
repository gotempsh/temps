use axum::extract::DefaultBodyLimit;
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    routing::post,
    Router,
};
use bytes::Bytes;
use serde::Serialize;
use std::sync::Arc;
use tracing::{error, info, warn};
use utoipa::ToSchema;

use super::types::GitAppState as AppState;

/// Register Bitbucket Cloud webhook routes.
///
/// Route: `POST /webhook/git/bitbucket/events/{delivery_token}`
///
/// The `{delivery_token}` path segment is the secret-in-path token generated
/// at webhook-connection time and stored encrypted in
/// `projects.bitbucket_webhook_token`. Bitbucket Cloud webhooks have no HMAC
/// body signature, so the URL token is the sole authentication mechanism.
///
/// Applies `DefaultBodyLimit::max(512 KiB)` (MUST-FIX 2 from the security
/// review). Webhook bodies are a few KB; the limit prevents memory exhaustion.
///
/// # Security (MUST-FIX 3)
/// The handler ALWAYS returns HTTP 200, regardless of:
/// - Whether `{delivery_token}` matches any project
/// - Whether a project is found
/// - Whether the body is parseable
///
/// This prevents Bitbucket (or an attacker who has enumerated a token) from
/// learning whether a given token is registered in Temps.
///
/// The token-bearing request path is NEVER logged.
pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/webhook/git/bitbucket/events/{delivery_token}",
        post(bitbucket_webhook_events).layer(DefaultBodyLimit::max(512 * 1024)),
    )
}

// ── Webhook handler ───────────────────────────────────────────────────────────

#[derive(Serialize, ToSchema)]
pub struct BitbucketWebhookResponse {
    message: String,
}

/// Receive Bitbucket Cloud push events and trigger Temps deployment pipelines.
///
/// # Authentication
/// Bitbucket Cloud webhooks have no HMAC body signature. Authentication is
/// performed via the `{delivery_token}` in the URL path:
///
/// 1. Fetch ALL project rows from the DB (never `WHERE token = ?` on ciphertext).
/// 2. Decrypt each stored `bitbucket_webhook_token` and compare to
///    `delivery_token` in **constant time** via `subtle::ConstantTimeEq`.
/// 3. Only dispatch if a match is found.
///
/// # Security properties
/// - Body limited to 512 KiB via `DefaultBodyLimit` at route registration
///   (MUST-FIX 2).
/// - Token lookup is constant-time (MUST-FIX 3, `subtle::ConstantTimeEq`).
/// - Returns **HTTP 200** on any failure (no existence oracle, MUST-FIX 3).
/// - `X-Hook-UUID` is logged for traceability; the delivery token path is
///   **never logged** (MUST-FIX 3).
///
/// # Event handling
/// - `repo:push` — dispatched to the existing push-event pipeline.
/// - `pullrequest:*` — logged and discarded (v1 deferred, ADR Decision 1b).
/// - All others — logged and discarded.
async fn bitbucket_webhook_events(
    // NOTE: Path MUST be extracted before logging anything to guarantee the
    // delivery_token never appears in a tracing span or log line.
    Path(delivery_token): Path<String>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> axum::Json<BitbucketWebhookResponse> {
    // X-Event-Key: repo:push, pullrequest:created, etc.
    let event_key = headers
        .get("X-Event-Key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    // Log the Hook UUID for traceability — NOT the delivery_token, NOT the path.
    let hook_uuid = headers
        .get("X-Hook-UUID")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    info!(
        hook_uuid = %hook_uuid,
        event_key = %event_key,
        "Received Bitbucket Cloud webhook event"
    );

    // Route on event type.
    // Handled:
    //   repo:push                           → existing push pipeline
    //   pullrequest:created                 → PR preview deploy pipeline
    //   pullrequest:updated                 → PR preview deploy pipeline
    // Discarded (non-fatal):
    //   pullrequest:fulfilled / :rejected   → logged, no action in v1
    //   everything else                     → logged, discarded
    let is_push = event_key == "repo:push";
    let is_pr_open = event_key == "pullrequest:created" || event_key == "pullrequest:updated";
    let is_pr_closed = event_key == "pullrequest:fulfilled" || event_key == "pullrequest:rejected";

    if !is_push && !is_pr_open && !is_pr_closed {
        info!(
            hook_uuid = %hook_uuid,
            event_key = %event_key,
            "Ignoring unhandled Bitbucket event type"
        );
        // ALWAYS return 200 — no existence oracle.
        return axum::Json(BitbucketWebhookResponse {
            message: "processed".to_string(),
        });
    }

    if is_pr_closed {
        // pullrequest:fulfilled / :rejected — log and discard.
        info!(
            hook_uuid = %hook_uuid,
            event_key = %event_key,
            "Ignoring Bitbucket pullrequest closed/merged event (no deploy action in v1)"
        );
        return axum::Json(BitbucketWebhookResponse {
            message: "processed".to_string(),
        });
    }

    // Parse the payload to extract repository info and commit SHA.
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            error!(
                hook_uuid = %hook_uuid,
                "Failed to parse Bitbucket webhook payload: {}",
                e
            );
            // ALWAYS return 200 — no existence oracle (MUST-FIX 3).
            return axum::Json(BitbucketWebhookResponse {
                message: "processed".to_string(),
            });
        }
    };

    // Extract repository.full_name ("workspace/repo_slug") from payload.
    let full_name = payload
        .get("repository")
        .and_then(|r| r.get("full_name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let (repo_owner, repo_name) = if let Some((owner, name)) = full_name.split_once('/') {
        (owner.to_string(), name.to_string())
    } else {
        // If full_name is missing/malformed, log and return 200 (MUST-FIX 3).
        warn!(
            hook_uuid = %hook_uuid,
            "Bitbucket webhook missing or malformed repository.full_name, ignoring"
        );
        return axum::Json(BitbucketWebhookResponse {
            message: "processed".to_string(),
        });
    };

    // Extract the ref/branch and commit SHA.
    // For push events, use the push.changes array.
    // For PR events, use the pullrequest.source.branch.name + source.commit.hash.
    let (git_ref, commit_sha) = if is_push {
        extract_push_ref_and_commit(&payload)
    } else {
        extract_pr_branch_and_commit(&payload)
    };

    // Constant-time secret-in-path lookup (MUST-FIX 3).
    let matched_projects = {
        let manager = state.git_provider_manager.clone();
        let token = delivery_token.clone();

        crate::services::bitbucket_provider::constant_time_token_lookup(
            manager.db(),
            &token,
            |project| {
                let enc_token = project.bitbucket_webhook_token.clone();
                let mgr = manager.clone();
                async move {
                    let encrypted = enc_token?;
                    mgr.decrypt_token(&encrypted).await.ok()
                }
            },
        )
        .await
    };

    if matched_projects.is_empty() {
        // SECURITY: return 200 even when no project matches — no existence oracle
        // (MUST-FIX 3). Warn in logs without including the delivery_token.
        warn!(
            hook_uuid = %hook_uuid,
            repo_owner = %repo_owner,
            repo_name = %repo_name,
            "No Temps project matched Bitbucket delivery token — ignoring"
        );
        return axum::Json(BitbucketWebhookResponse {
            message: "processed".to_string(),
        });
    }

    // Determine branch and tag from the git ref.
    let branch = if git_ref.starts_with("refs/heads/") {
        Some(git_ref.replace("refs/heads/", ""))
    } else if !git_ref.starts_with("refs/") && !git_ref.is_empty() {
        // Bitbucket PR events send the branch name directly (no refs/ prefix).
        Some(git_ref.clone())
    } else {
        None
    };

    let tag = if git_ref.starts_with("refs/tags/") {
        Some(git_ref.replace("refs/tags/", ""))
    } else {
        None
    };

    let authorized_projects =
        matched_projects_for_repository(matched_projects, repo_owner.as_str(), repo_name.as_str());

    if authorized_projects.is_empty() {
        warn!(
            hook_uuid = %hook_uuid,
            repo_owner = %repo_owner,
            repo_name = %repo_name,
            "Bitbucket webhook token matched projects for a different repository — ignoring"
        );
        return axum::Json(BitbucketWebhookResponse {
            message: "processed".to_string(),
        });
    }

    // Dispatch only to projects authenticated by the matched delivery token.
    let triggered = match state
        .git_provider_manager
        .handle_push_event_for_projects(
            branch.clone(),
            tag.clone(),
            commit_sha.clone(),
            authorized_projects,
        )
        .await
    {
        Ok(triggered) => triggered,
        Err(e) => {
            error!(
                hook_uuid = %hook_uuid,
                repo_owner = %repo_owner,
                repo_name = %repo_name,
                event_key = %event_key,
                "Failed to handle Bitbucket event: {:?}", e
            );
            0
        }
    };

    info!(
        hook_uuid = %hook_uuid,
        triggered = triggered,
        repo_owner = %repo_owner,
        repo_name = %repo_name,
        event_key = %event_key,
        "Bitbucket webhook event processed"
    );

    axum::Json(BitbucketWebhookResponse {
        message: format!("Processed {} project(s)", triggered),
    })
}

// ── Push payload helpers ──────────────────────────────────────────────────────

/// Extract the source branch name and head commit SHA from a Bitbucket
/// `pullrequest:created` or `pullrequest:updated` payload.
///
/// Bitbucket PR payload structure (relevant fields):
/// ```json
/// {
///   "pullrequest": {
///     "source": {
///       "branch": { "name": "feature/my-branch" },
///       "commit": { "hash": "abc123..." }
///     }
///   }
/// }
/// ```
///
/// Returns `(branch_name, commit_sha)` — NOT in `refs/heads/…` form because
/// the handler's `branch`-extraction logic already handles bare branch names.
fn extract_pr_branch_and_commit(payload: &serde_json::Value) -> (String, String) {
    let source = payload.get("pullrequest").and_then(|pr| pr.get("source"));

    let branch_name = source
        .and_then(|s| s.get("branch"))
        .and_then(|b| b.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let commit_sha = source
        .and_then(|s| s.get("commit"))
        .and_then(|c| c.get("hash"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    (branch_name, commit_sha)
}

/// Extract the git ref and commit SHA from a Bitbucket push payload.
///
/// Bitbucket push payload structure:
/// ```json
/// {
///   "push": {
///     "changes": [{
///       "new": {
///         "type": "branch",
///         "name": "main",
///         "target": { "hash": "abc123..." }
///       }
///     }]
///   }
/// }
/// ```
///
/// Returns `(ref_string, commit_sha)` where `ref_string` is in
/// `refs/heads/{branch}` form for branches or `refs/tags/{tag}` for tags.
fn extract_push_ref_and_commit(payload: &serde_json::Value) -> (String, String) {
    let changes = payload
        .get("push")
        .and_then(|p| p.get("changes"))
        .and_then(|c| c.as_array());

    let first_change = changes.and_then(|arr| arr.first());

    let new_ref = first_change.and_then(|c| c.get("new"));

    let ref_type = new_ref
        .and_then(|r| r.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("branch");

    let ref_name = new_ref
        .and_then(|r| r.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let commit_sha = new_ref
        .and_then(|r| r.get("target"))
        .and_then(|t| t.get("hash"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let git_ref = match ref_type {
        "tag" => format!("refs/tags/{}", ref_name),
        _ => format!("refs/heads/{}", ref_name),
    };

    (git_ref, commit_sha)
}

fn matched_projects_for_repository(
    projects: Vec<temps_entities::projects::Model>,
    repo_owner: &str,
    repo_name: &str,
) -> Vec<temps_entities::projects::Model> {
    projects
        .into_iter()
        .filter(|project| {
            project.repo_owner.eq_ignore_ascii_case(repo_owner)
                && project.repo_name.eq_ignore_ascii_case(repo_name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_project(id: i32, repo_owner: &str, repo_name: &str) -> temps_entities::projects::Model {
        let now = chrono::Utc::now();
        temps_entities::projects::Model {
            id,
            name: format!("project-{id}"),
            repo_name: repo_name.to_string(),
            repo_owner: repo_owner.to_string(),
            directory: "/".to_string(),
            main_branch: "main".to_string(),
            preset: temps_entities::preset::Preset::Vite,
            preset_config: None,
            deployment_config: None,
            created_at: now,
            updated_at: now,
            slug: format!("project-{id}"),
            is_deleted: false,
            deleted_at: None,
            last_deployment: None,
            is_public_repo: false,
            git_url: None,
            git_provider_connection_id: None,
            attack_mode: false,
            ai_alert_summaries_enabled: None,
            ai_debug_chat_enabled: None,
            ai_write_actions_enabled: false,
            cross_project_trace_sharing: true,
            enable_preview_environments: false,
            preview_envs_on_demand: false,
            preview_envs_idle_timeout_seconds: 300,
            preview_envs_wake_timeout_seconds: 30,
            source_type: temps_entities::source_type::SourceType::Git,
            gitlab_webhook_id: None,
            gitlab_webhook_signing_token: None,
            gitea_webhook_signing_token: None,
            bitbucket_webhook_token: None,
            bitbucket_webhook_hook_id: None,
            generic_webhook_token: None,
        }
    }

    #[test]
    fn test_matched_projects_for_repository_filters_token_matches_by_repo() {
        let projects = vec![
            test_project(1, "attacker", "demo"),
            test_project(2, "victim", "prod"),
        ];

        let authorized = matched_projects_for_repository(projects, "ATTACKER", "Demo");

        assert_eq!(authorized.len(), 1);
        assert_eq!(authorized[0].id, 1);
        assert_eq!(authorized[0].repo_owner, "attacker");
        assert_eq!(authorized[0].repo_name, "demo");
    }

    // ── extract_push_ref_and_commit ───────────────────────────────────────────

    #[test]
    fn test_extract_push_branch_ref() {
        let payload = serde_json::json!({
            "push": {
                "changes": [{
                    "new": {
                        "type": "branch",
                        "name": "main",
                        "target": { "hash": "abc123def456" }
                    }
                }]
            }
        });
        let (git_ref, sha) = extract_push_ref_and_commit(&payload);
        assert_eq!(git_ref, "refs/heads/main");
        assert_eq!(sha, "abc123def456");
    }

    #[test]
    fn test_extract_push_tag_ref() {
        let payload = serde_json::json!({
            "push": {
                "changes": [{
                    "new": {
                        "type": "tag",
                        "name": "v1.0.0",
                        "target": { "hash": "deadbeef" }
                    }
                }]
            }
        });
        let (git_ref, sha) = extract_push_ref_and_commit(&payload);
        assert_eq!(git_ref, "refs/tags/v1.0.0");
        assert_eq!(sha, "deadbeef");
    }

    #[test]
    fn test_extract_push_missing_changes_returns_empty() {
        let payload = serde_json::json!({
            "repository": { "full_name": "workspace/repo" }
        });
        let (git_ref, sha) = extract_push_ref_and_commit(&payload);
        assert_eq!(git_ref, "refs/heads/");
        assert_eq!(sha, "");
    }

    #[test]
    fn test_extract_push_empty_changes_array() {
        let payload = serde_json::json!({
            "push": { "changes": [] }
        });
        let (git_ref, sha) = extract_push_ref_and_commit(&payload);
        assert_eq!(git_ref, "refs/heads/");
        assert_eq!(sha, "");
    }

    // ── Branch/tag splitting from git_ref ─────────────────────────────────────

    #[test]
    fn test_refs_heads_prefix_stripped_to_branch() {
        let git_ref = "refs/heads/feature/my-branch".to_string();
        let branch = if git_ref.starts_with("refs/heads/") {
            Some(git_ref.replace("refs/heads/", ""))
        } else {
            None
        };
        assert_eq!(branch, Some("feature/my-branch".to_string()));
    }

    #[test]
    fn test_refs_tags_prefix_stripped_to_tag() {
        let git_ref = "refs/tags/v2.0.0".to_string();
        let tag = if git_ref.starts_with("refs/tags/") {
            Some(git_ref.replace("refs/tags/", ""))
        } else {
            None
        };
        assert_eq!(tag, Some("v2.0.0".to_string()));
    }

    // ── constant_time_token_lookup (property tests without DB) ────────────────

    #[test]
    fn test_delivery_token_matches_same_bytes() {
        use subtle::ConstantTimeEq;
        let token = "abc123deadbeef";
        assert_eq!(
            token.as_bytes().ct_eq(token.as_bytes()).unwrap_u8(),
            1,
            "same bytes must match"
        );
    }

    #[test]
    fn test_delivery_token_no_match_different_bytes() {
        use subtle::ConstantTimeEq;
        let a = "abc123";
        let b = "xyz987";
        assert_eq!(
            a.as_bytes().ct_eq(b.as_bytes()).unwrap_u8(),
            0,
            "different bytes must not match"
        );
    }

    // ── BitbucketWebhookResponse serialization ────────────────────────────────

    #[test]
    fn test_response_serializes_message() {
        let r = BitbucketWebhookResponse {
            message: "processed".to_string(),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"message\":\"processed\""));
    }

    // ── extract_pr_branch_and_commit ──────────────────────────────────────────

    #[test]
    fn test_extract_pr_branch_and_commit_full_payload() {
        let payload = serde_json::json!({
            "pullrequest": {
                "id": 5,
                "title": "My PR",
                "source": {
                    "branch": { "name": "feature/my-branch" },
                    "commit": { "hash": "deadbeef1234" }
                },
                "destination": {
                    "branch": { "name": "main" }
                }
            }
        });
        let (branch, sha) = extract_pr_branch_and_commit(&payload);
        assert_eq!(branch, "feature/my-branch");
        assert_eq!(sha, "deadbeef1234");
    }

    #[test]
    fn test_extract_pr_branch_and_commit_missing_pullrequest_returns_empty() {
        let payload = serde_json::json!({
            "repository": { "full_name": "workspace/repo" }
        });
        let (branch, sha) = extract_pr_branch_and_commit(&payload);
        assert_eq!(branch, "");
        assert_eq!(sha, "");
    }

    #[test]
    fn test_extract_pr_branch_and_commit_missing_commit_hash_returns_empty_sha() {
        let payload = serde_json::json!({
            "pullrequest": {
                "source": {
                    "branch": { "name": "feature/x" }
                }
            }
        });
        let (branch, sha) = extract_pr_branch_and_commit(&payload);
        assert_eq!(branch, "feature/x");
        assert_eq!(sha, "");
    }

    // ── Branch extraction from bare name (PR event path) ─────────────────────

    #[test]
    fn test_bare_branch_name_treated_as_branch() {
        // PR events send branch name directly (no refs/ prefix).
        let git_ref = "feature/my-branch".to_string();
        let branch = if git_ref.starts_with("refs/heads/") {
            Some(git_ref.replace("refs/heads/", ""))
        } else if !git_ref.starts_with("refs/") && !git_ref.is_empty() {
            Some(git_ref.clone())
        } else {
            None
        };
        assert_eq!(branch, Some("feature/my-branch".to_string()));
    }

    #[test]
    fn test_empty_git_ref_yields_no_branch() {
        let git_ref = "".to_string();
        let branch = if git_ref.starts_with("refs/heads/") {
            Some(git_ref.replace("refs/heads/", ""))
        } else if !git_ref.starts_with("refs/") && !git_ref.is_empty() {
            Some(git_ref.clone())
        } else {
            None
        };
        assert_eq!(branch, None);
    }
}
