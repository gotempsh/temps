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

/// Register Generic/Manual webhook routes.
///
/// Route: `POST /webhook/git/generic/events/{delivery_token}`
///
/// The `{delivery_token}` path segment is the secret-in-path token generated
/// at connection time and stored encrypted in `projects.generic_webhook_token`.
/// Generic providers have no REST API and no HMAC body signature, so the URL
/// token is the sole authentication mechanism.
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
/// This prevents any caller from learning whether a given token is registered
/// in Temps. The token-bearing request path is NEVER logged.
pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/webhook/git/generic/events/{delivery_token}",
        post(generic_webhook_events).layer(DefaultBodyLimit::max(512 * 1024)),
    )
}

// ── Webhook handler ───────────────────────────────────────────────────────────

#[derive(Serialize, ToSchema)]
pub struct GenericWebhookResponse {
    message: String,
}

/// Receive Generic/Manual git provider push events and trigger Temps deployment
/// pipelines.
///
/// # Authentication
/// Authentication is performed via the `{delivery_token}` in the URL path:
///
/// 1. Fetch ALL project rows from the DB (never `WHERE token = ?` on ciphertext).
/// 2. Decrypt each stored `generic_webhook_token` and compare to
///    `delivery_token` in **constant time** via `subtle::ConstantTimeEq`.
/// 3. Only dispatch if a match is found.
///
/// # Security properties
/// - Body limited to 512 KiB via `DefaultBodyLimit` at route registration
///   (MUST-FIX 2).
/// - Token lookup is constant-time (MUST-FIX 3, `subtle::ConstantTimeEq`).
/// - Returns **HTTP 200** on any failure (no existence oracle, MUST-FIX 3).
/// - Delivery identifier (if present in the body) is logged for traceability;
///   the delivery token path is **never logged** (MUST-FIX 3).
///
/// # Body format
/// Accepts any JSON body with a `ref` field (e.g., `"refs/heads/main"`).
/// This is the minimal contract shared by most git forge webhook payloads.
async fn generic_webhook_events(
    // NOTE: Path MUST be extracted before logging anything to guarantee the
    // delivery_token never appears in a tracing span or log line.
    Path(delivery_token): Path<String>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> axum::Json<GenericWebhookResponse> {
    // Log a delivery identifier for traceability — NOT the delivery_token,
    // NOT the path. Use X-Delivery or X-Hook-UUID if the sender provides one.
    let delivery_id = headers
        .get("X-Delivery")
        .or_else(|| headers.get("X-Hook-UUID"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    info!(
        delivery_id = %delivery_id,
        "Received Generic webhook event"
    );

    // Parse JSON body. We require a `ref` field per the spec.
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            error!(
                delivery_id = %delivery_id,
                "Failed to parse Generic webhook payload: {}",
                e
            );
            // ALWAYS return 200 — no existence oracle (MUST-FIX 3).
            return axum::Json(GenericWebhookResponse {
                message: "processed".to_string(),
            });
        }
    };

    // Extract the `ref` field — mandatory for dispatch.
    let git_ref = match payload.get("ref").and_then(|v| v.as_str()) {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => {
            warn!(
                delivery_id = %delivery_id,
                "Generic webhook body missing required 'ref' field — ignoring"
            );
            // ALWAYS return 200 — no existence oracle (MUST-FIX 3).
            return axum::Json(GenericWebhookResponse {
                message: "processed".to_string(),
            });
        }
    };

    // Constant-time secret-in-path lookup (MUST-FIX 3).
    // Fetch all project rows, decrypt each stored generic_webhook_token,
    // compare to delivery_token in constant time. Never SQL-filter by token.
    let matched_projects = {
        let manager = state.git_provider_manager.clone();
        let token = delivery_token.clone();

        crate::services::bitbucket_provider::constant_time_token_lookup(
            manager.db(),
            &token,
            |project| {
                let enc_token = project.generic_webhook_token.clone();
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
            delivery_id = %delivery_id,
            git_ref = %git_ref,
            "No Temps project matched Generic webhook delivery token — ignoring"
        );
        return axum::Json(GenericWebhookResponse {
            message: "processed".to_string(),
        });
    }

    // Derive branch/tag from the git ref.
    let branch = if git_ref.starts_with("refs/heads/") {
        Some(git_ref.replace("refs/heads/", ""))
    } else if !git_ref.starts_with("refs/") {
        // Some forges send just the branch name without the refs/heads/ prefix.
        Some(git_ref.clone())
    } else {
        None
    };

    let tag = if git_ref.starts_with("refs/tags/") {
        Some(git_ref.replace("refs/tags/", ""))
    } else {
        None
    };

    // Optional: extract commit SHA if present.
    let commit_sha = extract_commit_sha(&payload);

    // Optional: extract repo owner/name from payload (best-effort).
    let (repo_owner, repo_name) = extract_repo_info(&payload);

    // The secret delivery token is the primary project binding for Generic
    // webhooks. When the payload also supplies a repository identity, narrow
    // the token-matched set to it; otherwise retain the authenticated set so
    // minimal generic payloads continue to work.
    let authorized_projects = authorized_projects_for_repository_payload(
        matched_projects,
        repo_owner.as_str(),
        repo_name.as_str(),
    );

    if authorized_projects.is_empty() {
        warn!(
            delivery_id = %delivery_id,
            repo_owner = %repo_owner,
            repo_name = %repo_name,
            "Generic webhook token matched projects for a different repository — ignoring"
        );
        return axum::Json(GenericWebhookResponse {
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
                delivery_id = %delivery_id,
                repo_owner = %repo_owner,
                repo_name = %repo_name,
                "Failed to handle Generic webhook push event: {:?}", e
            );
            0
        }
    };

    info!(
        delivery_id = %delivery_id,
        triggered = triggered,
        git_ref = %git_ref,
        repo_owner = %repo_owner,
        repo_name = %repo_name,
        "Generic webhook event processed"
    );

    axum::Json(GenericWebhookResponse {
        message: format!("Processed {} project(s)", triggered),
    })
}

// ── Payload helpers ───────────────────────────────────────────────────────────

/// Extract a commit SHA from a generic JSON push payload.
///
/// Checks common locations used by various forge implementations:
/// - `after` (GitHub-style)
/// - `commits[0].id` (GitLab-style)
/// - `head_commit.id`
fn extract_commit_sha(payload: &serde_json::Value) -> String {
    // GitHub-style: `after`
    if let Some(sha) = payload.get("after").and_then(|v| v.as_str()) {
        if !sha.is_empty() && sha != "0000000000000000000000000000000000000000" {
            return sha.to_string();
        }
    }

    // GitLab / Gogs style: `commits[0].id`
    if let Some(sha) = payload
        .get("commits")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("id"))
        .and_then(|v| v.as_str())
    {
        if !sha.is_empty() {
            return sha.to_string();
        }
    }

    // head_commit.id
    if let Some(sha) = payload
        .get("head_commit")
        .and_then(|c| c.get("id"))
        .and_then(|v| v.as_str())
    {
        if !sha.is_empty() {
            return sha.to_string();
        }
    }

    String::new()
}

/// Extract repo owner and name from a generic JSON push payload (best-effort).
///
/// Checks common field paths:
/// - `repository.full_name` (GitHub-style, "owner/repo")
/// - `repository.namespace` + `repository.name` (GitLab-style)
/// - `repository.owner.name` + `repository.name`
fn extract_repo_info(payload: &serde_json::Value) -> (String, String) {
    let repo = match payload.get("repository") {
        Some(r) => r,
        None => return (String::new(), String::new()),
    };

    // GitHub-style full_name: "owner/repo"
    if let Some(full_name) = repo.get("full_name").and_then(|v| v.as_str()) {
        if let Some((owner, name)) = full_name.split_once('/') {
            return (owner.to_string(), name.to_string());
        }
    }

    // GitLab-style: namespace + name
    let name = repo
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let owner = repo
        .get("namespace")
        .and_then(|v| v.as_str())
        .or_else(|| {
            repo.get("owner")
                .and_then(|o| o.get("name").and_then(|v| v.as_str()))
        })
        .unwrap_or("")
        .to_string();

    (owner, name)
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

fn authorized_projects_for_repository_payload(
    projects: Vec<temps_entities::projects::Model>,
    repo_owner: &str,
    repo_name: &str,
) -> Vec<temps_entities::projects::Model> {
    if repo_owner.is_empty() || repo_name.is_empty() {
        projects
    } else {
        matched_projects_for_repository(projects, repo_owner, repo_name)
    }
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

    #[test]
    fn test_missing_repository_metadata_keeps_token_matches() {
        let projects = vec![test_project(1, "owner", "repo")];

        let authorized = authorized_projects_for_repository_payload(projects, "", "");

        assert_eq!(authorized.len(), 1);
        assert_eq!(authorized[0].id, 1);
    }

    // ── extract_commit_sha ────────────────────────────────────────────────────

    #[test]
    fn test_extract_commit_sha_github_style_after() {
        let payload = serde_json::json!({
            "ref": "refs/heads/main",
            "after": "abc123def456"
        });
        assert_eq!(extract_commit_sha(&payload), "abc123def456");
    }

    #[test]
    fn test_extract_commit_sha_gitlab_style_commits_array() {
        let payload = serde_json::json!({
            "ref": "refs/heads/main",
            "commits": [
                { "id": "deadbeef0000" }
            ]
        });
        assert_eq!(extract_commit_sha(&payload), "deadbeef0000");
    }

    #[test]
    fn test_extract_commit_sha_head_commit() {
        let payload = serde_json::json!({
            "ref": "refs/heads/main",
            "head_commit": { "id": "cafebabe1234" }
        });
        assert_eq!(extract_commit_sha(&payload), "cafebabe1234");
    }

    #[test]
    fn test_extract_commit_sha_deletion_push_returns_empty() {
        // All-zeros SHA means a deletion push — should be ignored
        let payload = serde_json::json!({
            "ref": "refs/heads/main",
            "after": "0000000000000000000000000000000000000000"
        });
        // Falls through to next strategy; no commits array → empty
        assert_eq!(extract_commit_sha(&payload), "");
    }

    #[test]
    fn test_extract_commit_sha_missing_returns_empty() {
        let payload = serde_json::json!({ "ref": "refs/heads/main" });
        assert_eq!(extract_commit_sha(&payload), "");
    }

    // ── extract_repo_info ─────────────────────────────────────────────────────

    #[test]
    fn test_extract_repo_info_github_full_name() {
        let payload = serde_json::json!({
            "repository": { "full_name": "myorg/myrepo" }
        });
        let (owner, name) = extract_repo_info(&payload);
        assert_eq!(owner, "myorg");
        assert_eq!(name, "myrepo");
    }

    #[test]
    fn test_extract_repo_info_gitlab_style() {
        let payload = serde_json::json!({
            "repository": {
                "name": "myrepo",
                "namespace": "mygroup"
            }
        });
        let (owner, name) = extract_repo_info(&payload);
        assert_eq!(owner, "mygroup");
        assert_eq!(name, "myrepo");
    }

    #[test]
    fn test_extract_repo_info_owner_name_nested() {
        let payload = serde_json::json!({
            "repository": {
                "name": "myrepo",
                "owner": { "name": "myuser" }
            }
        });
        let (owner, name) = extract_repo_info(&payload);
        assert_eq!(owner, "myuser");
        assert_eq!(name, "myrepo");
    }

    #[test]
    fn test_extract_repo_info_no_repository_field() {
        let payload = serde_json::json!({ "ref": "refs/heads/main" });
        let (owner, name) = extract_repo_info(&payload);
        assert_eq!(owner, "");
        assert_eq!(name, "");
    }

    // ── GenericWebhookResponse ────────────────────────────────────────────────

    #[test]
    fn test_response_serializes_message() {
        let r = GenericWebhookResponse {
            message: "processed".to_string(),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"message\":\"processed\""));
    }

    // ── Branch/tag splitting from git_ref ─────────────────────────────────────

    #[test]
    fn test_refs_heads_stripped_to_branch() {
        let git_ref = "refs/heads/feature/my-branch".to_string();
        let branch = if git_ref.starts_with("refs/heads/") {
            Some(git_ref.replace("refs/heads/", ""))
        } else {
            None
        };
        assert_eq!(branch, Some("feature/my-branch".to_string()));
    }

    #[test]
    fn test_refs_tags_stripped_to_tag() {
        let git_ref = "refs/tags/v1.2.3".to_string();
        let tag = if git_ref.starts_with("refs/tags/") {
            Some(git_ref.replace("refs/tags/", ""))
        } else {
            None
        };
        assert_eq!(tag, Some("v1.2.3".to_string()));
    }

    #[test]
    fn test_bare_branch_name_treated_as_branch() {
        // Some forges send just "main" without refs/heads/ prefix.
        let git_ref = "main".to_string();
        let branch = if git_ref.starts_with("refs/heads/") {
            Some(git_ref.replace("refs/heads/", ""))
        } else if !git_ref.starts_with("refs/") {
            Some(git_ref.clone())
        } else {
            None
        };
        assert_eq!(branch, Some("main".to_string()));
    }

    // ── Constant-time comparison (no-DB property tests) ───────────────────────

    #[test]
    fn test_constant_time_eq_matches() {
        use subtle::ConstantTimeEq;
        let a = b"my-generic-secret-token-12345678";
        let b = b"my-generic-secret-token-12345678";
        assert_eq!(a.ct_eq(b).unwrap_u8(), 1);
    }

    #[test]
    fn test_constant_time_eq_no_match() {
        use subtle::ConstantTimeEq;
        let a = b"token-aaa";
        let b = b"token-bbb";
        assert_eq!(a.ct_eq(b).unwrap_u8(), 0);
    }
}
