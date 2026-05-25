//! Pull request preview-deployment commenter.
//!
//! Vercel-style: when a deployment starts or finishes, look up the open PR
//! (or merge request) for the deployed branch and post or update a sticky
//! comment with the preview URL. The sticky comment is identified by an HTML
//! marker so subsequent updates edit in place rather than spamming the PR.
//!
//! Failures here are intentionally non-fatal: if a project has no git
//! provider, no open PR, or the API call fails, we log a warning and move
//! on. The deployment itself must never be blocked by a commenting failure.

use async_trait::async_trait;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, info, warn};

use super::git_provider_manager_trait::{GitProviderManagerError, GitProviderManagerTrait};

/// HTML marker embedded in every sticky comment so we can find and edit it
/// later. Scoped per `(project, environment)` so multiple Temps environments
/// targeting the same PR each get their own comment.
fn marker(project_id: i32, environment_id: i32) -> String {
    format!(
        "<!-- temps-preview:project={}:env={} -->",
        project_id, environment_id
    )
}

/// Lifecycle phase of the deployment used to render the comment body.
#[derive(Debug, Clone)]
pub enum CommentPhase {
    /// Build / deploy has started. `commit_short_sha` is the abbreviated SHA.
    Started { commit_short_sha: String },
    /// Deployment succeeded; `env_url` is the URL we want users to click.
    Ready {
        commit_short_sha: String,
        env_url: String,
        deployment_url: Option<String>,
    },
    /// Deployment failed; optional `deployment_url` links to logs.
    Failed {
        commit_short_sha: String,
        deployment_url: Option<String>,
    },
}

/// Context required to post a sticky PR/MR comment.
#[derive(Debug, Clone)]
pub struct PreviewCommentContext {
    pub project_id: i32,
    pub environment_id: i32,
    pub branch: String,
    pub phase: CommentPhase,
}

#[derive(Debug, Error)]
pub enum PrCommenterError {
    #[error("Project {project_id} has no git provider connection — skipping PR comment")]
    NoGitConnection { project_id: i32 },

    #[error("No open PR/MR found for branch '{branch}' on {owner}/{repo}")]
    NoOpenPullRequest {
        owner: String,
        repo: String,
        branch: String,
    },

    #[error("Git provider manager error: {0}")]
    Manager(#[from] GitProviderManagerError),

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("HTTP error calling {provider} for {owner}/{repo}: {reason}")]
    Http {
        provider: &'static str,
        owner: String,
        repo: String,
        reason: String,
    },

    #[error(
        "Forbidden by {provider} for {owner}/{repo}: missing scope or revoked token ({status})"
    )]
    Forbidden {
        provider: &'static str,
        owner: String,
        repo: String,
        status: u16,
    },

    #[error("Unsupported provider type for PR comments: {provider_type}")]
    UnsupportedProvider { provider_type: String },
}

/// Lightweight ref to an open PR/MR returned by the lookup step.
#[derive(Debug, Clone)]
pub struct OpenPullRequestRef {
    /// PR/MR number as the provider exposes it. For GitLab this is the
    /// MR `iid` (project-scoped), for GitHub the global PR number.
    pub number: i64,
}

#[async_trait]
pub trait PrCommenter: Send + Sync {
    /// Upsert a sticky preview comment on the open PR/MR for the deployment.
    /// Returns Ok(()) on success or when there's nothing to comment on
    /// (no PR, no git connection, etc. — those are logged at warn level,
    /// not propagated as errors, since they aren't deployment failures).
    async fn upsert_preview_comment(
        &self,
        ctx: PreviewCommentContext,
    ) -> Result<(), PrCommenterError>;
}

/// Production implementation backed by `GitProviderManagerTrait`.
pub struct GitPrCommenter {
    db: Arc<DatabaseConnection>,
    manager: Arc<dyn GitProviderManagerTrait>,
    http: reqwest::Client,
}

impl GitPrCommenter {
    pub fn new(db: Arc<DatabaseConnection>, manager: Arc<dyn GitProviderManagerTrait>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("Temps-PR-Commenter/1.0")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { db, manager, http }
    }

    /// Internal helper that does the real work; the trait method wraps this
    /// with the "log-and-swallow" graceful-degradation policy.
    async fn upsert_inner(&self, ctx: &PreviewCommentContext) -> Result<(), PrCommenterError> {
        use temps_entities::{git_provider_connections, git_providers, projects};

        let project = projects::Entity::find_by_id(ctx.project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| PrCommenterError::NoGitConnection {
                project_id: ctx.project_id,
            })?;

        let connection_id =
            project
                .git_provider_connection_id
                .ok_or(PrCommenterError::NoGitConnection {
                    project_id: ctx.project_id,
                })?;

        let connection = git_provider_connections::Entity::find_by_id(connection_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(GitProviderManagerError::ConnectionNotFound(connection_id))?;

        let provider = git_providers::Entity::find_by_id(connection.provider_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(GitProviderManagerError::ProviderNotFound(
                connection.provider_id,
            ))?;

        let (access_token, _provider_type) = self
            .manager
            .get_connection_access_token(connection_id)
            .await?;

        let body = render_body(ctx.project_id, ctx.environment_id, &ctx.phase);

        match provider.provider_type.as_str() {
            "github" => {
                let api_base = provider
                    .api_url
                    .clone()
                    .unwrap_or_else(|| "https://api.github.com".to_string());
                github_upsert(
                    &self.http,
                    &api_base,
                    &access_token,
                    &project.repo_owner,
                    &project.repo_name,
                    &ctx.branch,
                    &marker(ctx.project_id, ctx.environment_id),
                    &body,
                )
                .await
            }
            "gitlab" => {
                let api_base = provider
                    .api_url
                    .clone()
                    .or_else(|| provider.base_url.clone())
                    .unwrap_or_else(|| "https://gitlab.com".to_string());
                gitlab_upsert(
                    &self.http,
                    &api_base,
                    &access_token,
                    &project.repo_owner,
                    &project.repo_name,
                    &ctx.branch,
                    &marker(ctx.project_id, ctx.environment_id),
                    &body,
                )
                .await
            }
            other => Err(PrCommenterError::UnsupportedProvider {
                provider_type: other.to_string(),
            }),
        }
    }
}

#[async_trait]
impl PrCommenter for GitPrCommenter {
    async fn upsert_preview_comment(
        &self,
        ctx: PreviewCommentContext,
    ) -> Result<(), PrCommenterError> {
        match self.upsert_inner(&ctx).await {
            Ok(()) => {
                info!(
                    project_id = ctx.project_id,
                    environment_id = ctx.environment_id,
                    branch = %ctx.branch,
                    "Posted/updated PR preview comment"
                );
                Ok(())
            }
            // No PR / no connection / unsupported provider: not a deployment
            // failure, just nothing to do. Log and swallow.
            Err(e @ PrCommenterError::NoGitConnection { .. })
            | Err(e @ PrCommenterError::NoOpenPullRequest { .. })
            | Err(e @ PrCommenterError::UnsupportedProvider { .. }) => {
                debug!(
                    project_id = ctx.project_id,
                    environment_id = ctx.environment_id,
                    branch = %ctx.branch,
                    "Skipping PR preview comment: {}",
                    e
                );
                Ok(())
            }
            Err(e @ PrCommenterError::Forbidden { .. }) => {
                warn!(
                    project_id = ctx.project_id,
                    environment_id = ctx.environment_id,
                    branch = %ctx.branch,
                    "PR preview comment forbidden by provider — check installation permissions (pull_requests:write for GitHub Apps): {}",
                    e
                );
                Ok(())
            }
            Err(e) => {
                warn!(
                    project_id = ctx.project_id,
                    environment_id = ctx.environment_id,
                    branch = %ctx.branch,
                    "Failed to post PR preview comment: {}",
                    e
                );
                Err(e)
            }
        }
    }
}

fn render_body(project_id: i32, environment_id: i32, phase: &CommentPhase) -> String {
    let marker = marker(project_id, environment_id);
    match phase {
        CommentPhase::Started { commit_short_sha } => {
            format!("{marker}\n## 🚧 Deploying preview\n\nBuilding commit `{commit_short_sha}`…",)
        }
        CommentPhase::Ready {
            commit_short_sha,
            env_url,
            deployment_url,
        } => {
            let logs = deployment_url
                .as_ref()
                .map(|u| format!("\n\n[View deployment logs]({u})"))
                .unwrap_or_default();
            format!(
                "{marker}\n## ✅ Preview ready\n\n**Commit:** `{commit_short_sha}`\n\n🔗 **[Open preview]({env_url})**{logs}",
            )
        }
        CommentPhase::Failed {
            commit_short_sha,
            deployment_url,
        } => {
            let logs = deployment_url
                .as_ref()
                .map(|u| format!("\n\n[View deployment logs]({u})"))
                .unwrap_or_default();
            format!(
                "{marker}\n## ❌ Preview build failed\n\n**Commit:** `{commit_short_sha}`{logs}",
            )
        }
    }
}

// ===== GitHub =====

#[derive(Deserialize)]
struct GhPull {
    number: i64,
}

#[derive(Deserialize)]
struct GhComment {
    id: i64,
    #[serde(default)]
    body: String,
}

#[allow(clippy::too_many_arguments)]
async fn github_upsert(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    branch: &str,
    marker: &str,
    body: &str,
) -> Result<(), PrCommenterError> {
    let api_base = api_base.trim_end_matches('/');
    let pr = github_find_open_pr(http, api_base, token, owner, repo, branch).await?;

    let comments_url = format!(
        "{api_base}/repos/{owner}/{repo}/issues/{}/comments?per_page=100",
        pr.number
    );
    let resp = http
        .get(&comments_url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| PrCommenterError::Http {
            provider: "github",
            owner: owner.to_string(),
            repo: repo.to_string(),
            reason: format!("list comments: {e}"),
        })?;

    let status = resp.status();
    if status.as_u16() == 403 || status.as_u16() == 401 {
        return Err(PrCommenterError::Forbidden {
            provider: "github",
            owner: owner.to_string(),
            repo: repo.to_string(),
            status: status.as_u16(),
        });
    }
    if !status.is_success() {
        return Err(PrCommenterError::Http {
            provider: "github",
            owner: owner.to_string(),
            repo: repo.to_string(),
            reason: format!("list comments returned {status}"),
        });
    }

    let comments: Vec<GhComment> = resp.json().await.map_err(|e| PrCommenterError::Http {
        provider: "github",
        owner: owner.to_string(),
        repo: repo.to_string(),
        reason: format!("parse comments: {e}"),
    })?;

    let existing = comments.iter().find(|c| c.body.contains(marker));

    let payload = serde_json::json!({ "body": body });

    let result = if let Some(existing) = existing {
        let url = format!(
            "{api_base}/repos/{owner}/{repo}/issues/comments/{}",
            existing.id
        );
        http.patch(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .bearer_auth(token)
            .json(&payload)
            .send()
            .await
    } else {
        let url = format!(
            "{api_base}/repos/{owner}/{repo}/issues/{}/comments",
            pr.number
        );
        http.post(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .bearer_auth(token)
            .json(&payload)
            .send()
            .await
    };

    let resp = result.map_err(|e| PrCommenterError::Http {
        provider: "github",
        owner: owner.to_string(),
        repo: repo.to_string(),
        reason: format!("upsert: {e}"),
    })?;

    let status = resp.status();
    if status.as_u16() == 403 || status.as_u16() == 401 {
        return Err(PrCommenterError::Forbidden {
            provider: "github",
            owner: owner.to_string(),
            repo: repo.to_string(),
            status: status.as_u16(),
        });
    }
    if !status.is_success() {
        return Err(PrCommenterError::Http {
            provider: "github",
            owner: owner.to_string(),
            repo: repo.to_string(),
            reason: format!("upsert returned {status}"),
        });
    }
    Ok(())
}

async fn github_find_open_pr(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    branch: &str,
) -> Result<OpenPullRequestRef, PrCommenterError> {
    // GitHub's `head` filter on the pulls list uses `user:branch` form. The
    // user is the *head* repo owner — for a same-repo branch this is the
    // repo owner; for forks it would differ, but Temps deploys are
    // configured per-repo so we only ever comment on same-repo PRs.
    let head = format!("{owner}:{branch}");
    let url = format!(
        "{api_base}/repos/{owner}/{repo}/pulls?state=open&head={}&per_page=1",
        urlencoding::encode(&head)
    );

    let resp = http
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| PrCommenterError::Http {
            provider: "github",
            owner: owner.to_string(),
            repo: repo.to_string(),
            reason: format!("find PR: {e}"),
        })?;

    let status = resp.status();
    if status.as_u16() == 403 || status.as_u16() == 401 {
        return Err(PrCommenterError::Forbidden {
            provider: "github",
            owner: owner.to_string(),
            repo: repo.to_string(),
            status: status.as_u16(),
        });
    }
    if !status.is_success() {
        return Err(PrCommenterError::Http {
            provider: "github",
            owner: owner.to_string(),
            repo: repo.to_string(),
            reason: format!("find PR returned {status}"),
        });
    }

    let pulls: Vec<GhPull> = resp.json().await.map_err(|e| PrCommenterError::Http {
        provider: "github",
        owner: owner.to_string(),
        repo: repo.to_string(),
        reason: format!("parse PRs: {e}"),
    })?;

    pulls
        .into_iter()
        .next()
        .map(|p| OpenPullRequestRef { number: p.number })
        .ok_or_else(|| PrCommenterError::NoOpenPullRequest {
            owner: owner.to_string(),
            repo: repo.to_string(),
            branch: branch.to_string(),
        })
}

// ===== GitLab =====

#[derive(Deserialize)]
struct GlMr {
    iid: i64,
}

#[derive(Deserialize)]
struct GlNote {
    id: i64,
    #[serde(default)]
    body: String,
}

#[allow(clippy::too_many_arguments)]
async fn gitlab_upsert(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    branch: &str,
    marker: &str,
    body: &str,
) -> Result<(), PrCommenterError> {
    let api_base = api_base.trim_end_matches('/');
    let project_path = format!("{owner}/{repo}");
    let project_id_encoded = urlencoding::encode(&project_path).into_owned();

    let mr = gitlab_find_open_mr(
        http,
        api_base,
        token,
        &project_id_encoded,
        owner,
        repo,
        branch,
    )
    .await?;

    let notes_url = format!(
        "{api_base}/api/v4/projects/{project_id_encoded}/merge_requests/{}/notes?per_page=100",
        mr.iid
    );
    // GitLab accepts both PRIVATE-TOKEN (PAT) and Bearer (OAuth). We don't
    // know which kind of token the connection holds without inspecting the
    // provider's auth_method, so we set PRIVATE-TOKEN unconditionally — it
    // works for PATs and is ignored when Authorization is also valid for
    // OAuth tokens used via the same header is also accepted. To keep the
    // OAuth case working we ALSO try the Authorization: Bearer header.
    let resp = http
        .get(&notes_url)
        .header("PRIVATE-TOKEN", token)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| PrCommenterError::Http {
            provider: "gitlab",
            owner: owner.to_string(),
            repo: repo.to_string(),
            reason: format!("list notes: {e}"),
        })?;

    let status = resp.status();
    if status.as_u16() == 403 || status.as_u16() == 401 {
        return Err(PrCommenterError::Forbidden {
            provider: "gitlab",
            owner: owner.to_string(),
            repo: repo.to_string(),
            status: status.as_u16(),
        });
    }
    if !status.is_success() {
        return Err(PrCommenterError::Http {
            provider: "gitlab",
            owner: owner.to_string(),
            repo: repo.to_string(),
            reason: format!("list notes returned {status}"),
        });
    }

    let notes: Vec<GlNote> = resp.json().await.map_err(|e| PrCommenterError::Http {
        provider: "gitlab",
        owner: owner.to_string(),
        repo: repo.to_string(),
        reason: format!("parse notes: {e}"),
    })?;

    let existing = notes.iter().find(|n| n.body.contains(marker));
    let payload = serde_json::json!({ "body": body });

    let result = if let Some(existing) = existing {
        let url = format!(
            "{api_base}/api/v4/projects/{project_id_encoded}/merge_requests/{}/notes/{}",
            mr.iid, existing.id
        );
        http.put(&url)
            .header("PRIVATE-TOKEN", token)
            .header("Authorization", format!("Bearer {token}"))
            .json(&payload)
            .send()
            .await
    } else {
        let url = format!(
            "{api_base}/api/v4/projects/{project_id_encoded}/merge_requests/{}/notes",
            mr.iid
        );
        http.post(&url)
            .header("PRIVATE-TOKEN", token)
            .header("Authorization", format!("Bearer {token}"))
            .json(&payload)
            .send()
            .await
    };

    let resp = result.map_err(|e| PrCommenterError::Http {
        provider: "gitlab",
        owner: owner.to_string(),
        repo: repo.to_string(),
        reason: format!("upsert: {e}"),
    })?;

    let status = resp.status();
    if status.as_u16() == 403 || status.as_u16() == 401 {
        return Err(PrCommenterError::Forbidden {
            provider: "gitlab",
            owner: owner.to_string(),
            repo: repo.to_string(),
            status: status.as_u16(),
        });
    }
    if !status.is_success() {
        return Err(PrCommenterError::Http {
            provider: "gitlab",
            owner: owner.to_string(),
            repo: repo.to_string(),
            reason: format!("upsert returned {status}"),
        });
    }
    Ok(())
}

async fn gitlab_find_open_mr(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    project_id_encoded: &str,
    owner: &str,
    repo: &str,
    branch: &str,
) -> Result<GlMr, PrCommenterError> {
    let url = format!(
        "{api_base}/api/v4/projects/{project_id_encoded}/merge_requests?state=opened&source_branch={}&per_page=1",
        urlencoding::encode(branch)
    );

    let resp = http
        .get(&url)
        .header("PRIVATE-TOKEN", token)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| PrCommenterError::Http {
            provider: "gitlab",
            owner: owner.to_string(),
            repo: repo.to_string(),
            reason: format!("find MR: {e}"),
        })?;

    let status = resp.status();
    if status.as_u16() == 403 || status.as_u16() == 401 {
        return Err(PrCommenterError::Forbidden {
            provider: "gitlab",
            owner: owner.to_string(),
            repo: repo.to_string(),
            status: status.as_u16(),
        });
    }
    if !status.is_success() {
        return Err(PrCommenterError::Http {
            provider: "gitlab",
            owner: owner.to_string(),
            repo: repo.to_string(),
            reason: format!("find MR returned {status}"),
        });
    }

    let mrs: Vec<GlMr> = resp.json().await.map_err(|e| PrCommenterError::Http {
        provider: "gitlab",
        owner: owner.to_string(),
        repo: repo.to_string(),
        reason: format!("parse MRs: {e}"),
    })?;

    mrs.into_iter()
        .next()
        .ok_or_else(|| PrCommenterError::NoOpenPullRequest {
            owner: owner.to_string(),
            repo: repo.to_string(),
            branch: branch.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    fn ctx(phase: CommentPhase) -> PreviewCommentContext {
        PreviewCommentContext {
            project_id: 42,
            environment_id: 7,
            branch: "feature/x".to_string(),
            phase,
        }
    }

    #[test]
    fn render_started_includes_marker_and_sha() {
        let body = render_body(
            42,
            7,
            &CommentPhase::Started {
                commit_short_sha: "abc1234".to_string(),
            },
        );
        assert!(body.contains("<!-- temps-preview:project=42:env=7 -->"));
        assert!(body.contains("abc1234"));
        assert!(body.contains("Deploying preview"));
    }

    #[test]
    fn render_ready_includes_env_url() {
        let body = render_body(
            42,
            7,
            &CommentPhase::Ready {
                commit_short_sha: "abc1234".to_string(),
                env_url: "https://feature-x.preview.temps.app".to_string(),
                deployment_url: Some("https://dashboard.temps.app/d/1".to_string()),
            },
        );
        assert!(body.contains("https://feature-x.preview.temps.app"));
        assert!(body.contains("Preview ready"));
        assert!(body.contains("dashboard.temps.app"));
    }

    #[test]
    fn render_failed_works_without_log_url() {
        let body = render_body(
            42,
            7,
            &CommentPhase::Failed {
                commit_short_sha: "abc1234".to_string(),
                deployment_url: None,
            },
        );
        assert!(body.contains("Preview build failed"));
        assert!(!body.contains("View deployment logs"));
    }

    #[test]
    fn marker_is_scoped_per_env() {
        assert_ne!(marker(42, 7), marker(42, 8));
        assert_ne!(marker(42, 7), marker(43, 7));
    }

    #[tokio::test]
    async fn github_upsert_creates_new_comment_when_marker_absent() {
        let mut server = Server::new_async().await;

        let find_pr = server
            .mock("GET", "/repos/octo/hello/pulls")
            .match_query(mockito::Matcher::AllOf(vec![mockito::Matcher::UrlEncoded(
                "state".into(),
                "open".into(),
            )]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"number": 99}]"#)
            .create_async()
            .await;

        let list_comments = server
            .mock("GET", "/repos/octo/hello/issues/99/comments")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"id": 1, "body": "unrelated comment"}]"#)
            .create_async()
            .await;

        let post_comment = server
            .mock("POST", "/repos/octo/hello/issues/99/comments")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id": 555}"#)
            .create_async()
            .await;

        let http = reqwest::Client::new();
        let res = github_upsert(
            &http,
            &server.url(),
            "tok",
            "octo",
            "hello",
            "feature/x",
            "<!-- temps-preview:project=42:env=7 -->",
            "hello world",
        )
        .await;

        assert!(res.is_ok(), "expected ok, got {:?}", res);
        find_pr.assert_async().await;
        list_comments.assert_async().await;
        post_comment.assert_async().await;
    }

    #[tokio::test]
    async fn github_upsert_edits_existing_comment_when_marker_present() {
        let mut server = Server::new_async().await;

        server
            .mock("GET", "/repos/octo/hello/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"[{"number": 99}]"#)
            .create_async()
            .await;

        server
            .mock("GET", "/repos/octo/hello/issues/99/comments")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(
                r#"[{"id": 42, "body": "old content\n<!-- temps-preview:project=42:env=7 -->"}]"#,
            )
            .create_async()
            .await;

        let patch = server
            .mock("PATCH", "/repos/octo/hello/issues/comments/42")
            .with_status(200)
            .with_body(r#"{"id": 42}"#)
            .create_async()
            .await;

        let http = reqwest::Client::new();
        let res = github_upsert(
            &http,
            &server.url(),
            "tok",
            "octo",
            "hello",
            "feature/x",
            "<!-- temps-preview:project=42:env=7 -->",
            "updated body",
        )
        .await;

        assert!(res.is_ok(), "expected ok, got {:?}", res);
        patch.assert_async().await;
    }

    #[tokio::test]
    async fn github_returns_no_open_pull_request_when_list_empty() {
        let mut server = Server::new_async().await;

        server
            .mock("GET", "/repos/octo/hello/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body("[]")
            .create_async()
            .await;

        let http = reqwest::Client::new();
        let res = github_upsert(
            &http,
            &server.url(),
            "tok",
            "octo",
            "hello",
            "feature/x",
            "<!-- temps-preview:project=42:env=7 -->",
            "body",
        )
        .await;

        assert!(matches!(
            res,
            Err(PrCommenterError::NoOpenPullRequest { .. })
        ));
    }

    #[tokio::test]
    async fn github_returns_forbidden_on_403() {
        let mut server = Server::new_async().await;

        server
            .mock("GET", "/repos/octo/hello/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(403)
            .with_body(r#"{"message":"Resource not accessible by integration"}"#)
            .create_async()
            .await;

        let http = reqwest::Client::new();
        let res = github_upsert(
            &http,
            &server.url(),
            "tok",
            "octo",
            "hello",
            "feature/x",
            "<!-- temps-preview:project=42:env=7 -->",
            "body",
        )
        .await;

        assert!(matches!(
            res,
            Err(PrCommenterError::Forbidden { status: 403, .. })
        ));
    }

    #[tokio::test]
    async fn gitlab_upsert_creates_new_note_when_marker_absent() {
        let mut server = Server::new_async().await;

        let find_mr = server
            .mock("GET", "/api/v4/projects/octo%2Fhello/merge_requests")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"[{"iid": 12}]"#)
            .create_async()
            .await;

        let list_notes = server
            .mock(
                "GET",
                "/api/v4/projects/octo%2Fhello/merge_requests/12/notes",
            )
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"[{"id": 1, "body": "unrelated"}]"#)
            .create_async()
            .await;

        let post_note = server
            .mock(
                "POST",
                "/api/v4/projects/octo%2Fhello/merge_requests/12/notes",
            )
            .with_status(201)
            .with_body(r#"{"id": 9}"#)
            .create_async()
            .await;

        let http = reqwest::Client::new();
        let res = gitlab_upsert(
            &http,
            &server.url(),
            "tok",
            "octo",
            "hello",
            "feature/x",
            "<!-- temps-preview:project=42:env=7 -->",
            "body",
        )
        .await;

        assert!(res.is_ok(), "expected ok, got {:?}", res);
        find_mr.assert_async().await;
        list_notes.assert_async().await;
        post_note.assert_async().await;
    }

    #[tokio::test]
    async fn gitlab_upsert_edits_existing_note_when_marker_present() {
        let mut server = Server::new_async().await;

        server
            .mock("GET", "/api/v4/projects/octo%2Fhello/merge_requests")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"[{"iid": 12}]"#)
            .create_async()
            .await;

        server
            .mock(
                "GET",
                "/api/v4/projects/octo%2Fhello/merge_requests/12/notes",
            )
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"[{"id": 77, "body": "old\n<!-- temps-preview:project=42:env=7 -->"}]"#)
            .create_async()
            .await;

        let put = server
            .mock(
                "PUT",
                "/api/v4/projects/octo%2Fhello/merge_requests/12/notes/77",
            )
            .with_status(200)
            .with_body(r#"{"id": 77}"#)
            .create_async()
            .await;

        let http = reqwest::Client::new();
        let res = gitlab_upsert(
            &http,
            &server.url(),
            "tok",
            "octo",
            "hello",
            "feature/x",
            "<!-- temps-preview:project=42:env=7 -->",
            "body",
        )
        .await;

        assert!(res.is_ok(), "expected ok, got {:?}", res);
        put.assert_async().await;
    }

    #[tokio::test]
    async fn gitlab_returns_no_open_mr_when_list_empty() {
        let mut server = Server::new_async().await;

        server
            .mock("GET", "/api/v4/projects/octo%2Fhello/merge_requests")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body("[]")
            .create_async()
            .await;

        let http = reqwest::Client::new();
        let res = gitlab_upsert(
            &http,
            &server.url(),
            "tok",
            "octo",
            "hello",
            "feature/x",
            "<!-- temps-preview:project=42:env=7 -->",
            "body",
        )
        .await;

        assert!(matches!(
            res,
            Err(PrCommenterError::NoOpenPullRequest { .. })
        ));
    }

    /// Trivial smoke test that the context struct round-trips through the
    /// trait method dispatch in graceful-degradation mode.
    struct NoopCommenter;
    #[async_trait]
    impl PrCommenter for NoopCommenter {
        async fn upsert_preview_comment(
            &self,
            _ctx: PreviewCommentContext,
        ) -> Result<(), PrCommenterError> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn noop_commenter_succeeds() {
        let c = NoopCommenter;
        let res = c
            .upsert_preview_comment(ctx(CommentPhase::Started {
                commit_short_sha: "abc1234".into(),
            }))
            .await;
        assert!(res.is_ok());
    }
}
