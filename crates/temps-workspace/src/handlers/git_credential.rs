//! HTTP endpoint that mints per-operation git credentials for the
//! in-sandbox credential daemon. See
//! [`crate::services::git_credential_service`] for the policy.
//!
//! Auth model: deployment-token only. The daemon authenticates with the
//! workspace session's `TEMPS_API_TOKEN` (`Authorization: Bearer dt_...`),
//! and we extract `project_id` from the token — never from the request
//! body — so a daemon physically cannot ask for credentials belonging to
//! a different project.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use temps_auth::RequireAuth;
use temps_core::problemdetails::{self, Problem};
use tracing::{debug, info, warn};
use utoipa::ToSchema;

use crate::handlers::WorkspaceAppState;
use crate::services::git_credential_service::GitCredentialOperation;

/// Request body. Matches the parts of the git credential helper protocol
/// we care about: `host` and `path` (which we split into `owner` + `repo`
/// — the helper sends `path=owner/repo`).
#[derive(Debug, Deserialize, ToSchema)]
pub struct MintGitCredentialRequest {
    /// Git host: `github.com`, `gitlab.com`, etc. The host's allow-list is
    /// enforced server-side; unknown hosts are rejected.
    pub host: String,
    /// Repository owner (organization or user) part of `owner/repo`.
    pub owner: String,
    /// Repository name part of `owner/repo`.
    pub repo: String,
    /// Operation the credential will be used for. Drives permission
    /// narrowing on the minted token. Defaults to `fetch` so accidental
    /// requests can't escalate to write.
    #[serde(default)]
    pub operation: MintOperation,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MintOperation {
    /// `git clone` / `git fetch` / `git ls-remote`. Token gets
    /// `contents:read`.
    #[default]
    Fetch,
    /// `git push`. Token gets `contents:write`.
    Push,
}

impl From<MintOperation> for GitCredentialOperation {
    fn from(op: MintOperation) -> Self {
        match op {
            MintOperation::Fetch => GitCredentialOperation::Fetch,
            MintOperation::Push => GitCredentialOperation::Push,
        }
    }
}

/// Response shape. Matches the git credential helper's `get` output:
/// `username` and `password` are written verbatim into git's stdin so it
/// uses HTTP Basic. `expires_at` is informational — the daemon doesn't
/// reuse tokens across operations, so it ignores expiry — but we surface
/// it for audit log correlation.
#[derive(Debug, Serialize, ToSchema)]
pub struct MintGitCredentialResponse {
    pub username: String,
    /// Short-lived (≤1 hour) installation token. Never logged, never
    /// stored on disk by the helper or the daemon.
    pub password: String,
    /// RFC 3339 expiry timestamp, when the upstream provider reported one.
    /// Daemon should not depend on this — every operation gets a fresh
    /// mint anyway.
    pub expires_at: Option<String>,
}

/// `POST /workspace/git-credential` — mint a single-repo single-op token.
///
/// **403 Cross-project:** Caller is authenticated with a deployment token
/// for project A but asked for credentials for project B's repo. We never
/// trust a `project_id` field from the request — it's read from the token
/// only.
///
/// **403 Cross-repo:** Caller asked for credentials for a repo other than
/// the one the project is configured against. Defends against a
/// compromised daemon trying to enumerate every repo the underlying
/// GitHub App can reach.
///
/// **409 No connection:** Project exists but has no
/// `git_provider_connection_id` set.
///
/// **502 Mint failed:** Upstream provider couldn't issue a scoped token
/// (typically: PAT-backed connection that doesn't support narrowing, or
/// transient GitHub API failure).
#[utoipa::path(
    tag = "Workspace",
    post,
    path = "/workspace/git-credential",
    request_body = MintGitCredentialRequest,
    responses(
        (status = 200, description = "Token minted", body = MintGitCredentialResponse),
        (status = 401, description = "Missing or invalid deployment token"),
        (status = 403, description = "Repo not owned by caller's project, or unknown host"),
        (status = 409, description = "Project has no git provider connection"),
        (status = 502, description = "Upstream mint failed"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn mint_git_credential(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<WorkspaceAppState>>,
    Json(request): Json<MintGitCredentialRequest>,
) -> Result<impl IntoResponse, Problem> {
    // 1. Caller MUST be a deployment token. Sessions and API keys must
    // not be allowed to call this — they don't carry the same "physically
    // bound to one project at issue time" property, so the cross-project
    // guarantee would slip. Refuse with 403, not 401, because the request
    // *was* authenticated — just with the wrong kind of credential.
    let project_id = match auth.project_id() {
        Some(id) => id,
        None => {
            warn!("git-credential mint refused: caller is not a deployment token");
            return Err(problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Deployment Token Required")
                .with_detail(
                    "This endpoint can only be called with a workspace session deployment token; \
                     user sessions and API keys are not accepted."
                        .to_string(),
                ));
        }
    };

    // The credential service unwraps to a typed WorkspaceError, which the
    // existing `From<WorkspaceError> for Problem` (in handlers/sessions.rs)
    // maps to the right status codes (403/409/502/etc.).
    let grant = app_state
        .git_credential_service
        .as_ref()
        .ok_or_else(|| {
            problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                .with_title("Git Credential Service Unavailable")
                .with_detail(
                    "Workspace credential minting is not configured on this server. \
                     This usually means the git plugin failed to register."
                        .to_string(),
                )
        })?
        .mint_for_project(
            project_id,
            &request.host,
            &request.owner,
            &request.repo,
            request.operation.into(),
        )
        .await?;

    // Audit-log the mint. Includes everything we'd need to investigate a
    // suspected leak: which project, which repo, which operation, when.
    // Intentionally does NOT include the token itself.
    info!(
        project_id,
        host = %request.host,
        owner = %request.owner,
        repo = %request.repo,
        operation = ?request.operation,
        expires_at = ?grant.expires_at,
        "Minted scoped git credential"
    );

    debug!(
        project_id,
        owner = %request.owner,
        repo = %request.repo,
        "Returning scoped credential to in-sandbox daemon"
    );

    Ok((
        StatusCode::OK,
        Json(MintGitCredentialResponse {
            username: grant.username,
            password: grant.password,
            expires_at: grant
                .expires_at
                .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        }),
    ))
}

/// Routes — single endpoint, no path parameters (project comes from the
/// deployment token).
pub fn routes() -> Router<Arc<WorkspaceAppState>> {
    Router::new().route("/workspace/git-credential", post(mint_git_credential))
}
