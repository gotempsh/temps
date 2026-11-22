// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Issues and commits of a project's repository, for error tracking
//! (ADR 111, D5), through the git provider connection the project deploys
//! with. GitHub and GitLab (including self-hosted instances) are reached;
//! other providers report [`SourceControlError::Unsupported`].
//!
//! Every call has a 10 second timeout, follows no redirects and resolves
//! hosts through the outbound guard, like every other provider call.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{DatabaseConnection, EntityTrait};
use serde::Deserialize;
use temps_core::retry::RetryConfig;
use temps_core::source_control::{
    IssueState, ProjectRepository, ProjectSourceControl, RepositoryCommit, RepositoryIssue,
    SourceControlError,
};
use tracing::warn;

use super::git_provider::guarded_client_builder;
use super::git_provider_manager_trait::GitProviderManagerTrait;

/// Longest response body read from a provider.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Most commits one call returns.
pub const MAX_COMMITS: usize = 20;

/// Attempts for a read, the first included.
const READ_ATTEMPTS: u32 = 3;

/// The GitLab instance root the calls add `/api/v4` to. A provider's
/// `api_url` is stored with `/api/v4` (as the GitLab provider is set up)
/// or without it; either reaches the same root.
fn gitlab_root(api_url: Option<&str>, base_url: Option<&str>) -> String {
    let root = api_url
        .or(base_url)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .unwrap_or("https://gitlab.com")
        .trim_end_matches('/');
    root.strip_suffix("/api/v4")
        .unwrap_or(root)
        .trim_end_matches('/')
        .to_string()
}

/// One failed attempt, and whether trying again could succeed.
struct Attempt {
    error: SourceControlError,
    retryable: bool,
}

impl std::fmt::Display for Attempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

/// Which API a repository is reached through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavor {
    GitHub,
    GitLab,
}

impl Flavor {
    fn name(self) -> &'static str {
        match self {
            Self::GitHub => "github",
            Self::GitLab => "gitlab",
        }
    }
}

/// One repository and the credential to reach it.
#[derive(Debug, Clone)]
struct Api {
    project_id: i32,
    flavor: Flavor,
    /// GitHub: `https://api.github.com`. GitLab: the instance root; the
    /// calls add `/api/v4`.
    api_base: String,
    token: String,
    owner: String,
    repo: String,
    /// How a read is retried after a connection failure, a 429 or a 5xx.
    /// Writes (opening an issue) are never retried: a retry after a lost
    /// answer would open a second issue.
    retry: RetryConfig,
}

impl Api {
    fn repository(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    fn gitlab_project(&self) -> String {
        urlencoding::encode(&self.repository()).into_owned()
    }

    fn request(
        &self,
        http: &reqwest::Client,
        method: reqwest::Method,
        url: &str,
    ) -> reqwest::RequestBuilder {
        let builder = http.request(method, url);
        match self.flavor {
            Flavor::GitHub => builder
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .bearer_auth(&self.token),
            // GitLab accepts a personal token as PRIVATE-TOKEN and an OAuth
            // token as a bearer; the connection may hold either.
            Flavor::GitLab => builder
                .header("PRIVATE-TOKEN", &self.token)
                .bearer_auth(&self.token),
        }
    }

    fn provider_error(&self, operation: &'static str, reason: String) -> SourceControlError {
        SourceControlError::Provider {
            project_id: self.project_id,
            provider: self.flavor.name().into(),
            repository: self.repository(),
            operation,
            reason,
        }
    }

    /// Send the request once and decode a successful JSON answer. `what`
    /// names the thing a 404 is missing.
    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        operation: &'static str,
        what: &str,
    ) -> Result<T, SourceControlError> {
        self.attempt(request, operation, what)
            .await
            .map_err(|attempt| attempt.error)
    }

    /// Send a read, trying again after a connection failure, a 429 or a
    /// 5xx with backoff. A refused token, a missing issue or an answer that
    /// does not decode is returned at once.
    async fn send_read<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        operation: &'static str,
        what: &str,
    ) -> Result<T, SourceControlError> {
        self.retry
            .retry_if(
                || async {
                    let Some(request) = request.try_clone() else {
                        return Err(Attempt {
                            error: self
                                .provider_error(operation, "the request cannot be repeated".into()),
                            retryable: false,
                        });
                    };
                    self.attempt(request, operation, what).await
                },
                |attempt| attempt.retryable,
            )
            .await
            .map_err(|attempt| attempt.error)
    }

    async fn attempt<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        operation: &'static str,
        what: &str,
    ) -> Result<T, Attempt> {
        let fail = |error: SourceControlError| Attempt {
            error,
            retryable: false,
        };
        let response = request.send().await.map_err(|error| Attempt {
            error: self.provider_error(operation, error.to_string()),
            retryable: true,
        })?;
        let status = response.status();
        if status == reqwest::StatusCode::FORBIDDEN {
            if let Some(permission) = accepted_permissions(response.headers()) {
                return Err(fail(SourceControlError::MissingPermission {
                    project_id: self.project_id,
                    provider: self.flavor.name().into(),
                    repository: self.repository(),
                    operation,
                    permission,
                }));
            }
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(fail(SourceControlError::Forbidden {
                project_id: self.project_id,
                provider: self.flavor.name().into(),
                repository: self.repository(),
                operation,
                status: status.as_u16(),
            }));
        }
        // GitHub answers 422 for a commit it does not know.
        if status == reqwest::StatusCode::NOT_FOUND
            || (status == reqwest::StatusCode::UNPROCESSABLE_ENTITY && operation == "list commits")
        {
            return Err(fail(SourceControlError::NotFound {
                project_id: self.project_id,
                provider: self.flavor.name().into(),
                repository: self.repository(),
                what: what.to_string(),
            }));
        }
        if !status.is_success() {
            return Err(Attempt {
                error: self.provider_error(operation, format!("HTTP {status}")),
                retryable: status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || status.is_server_error(),
            });
        }
        let body = read_capped(response).await.map_err(|reason| Attempt {
            error: self.provider_error(operation, reason),
            retryable: false,
        })?;
        serde_json::from_slice(&body).map_err(|error| {
            fail(self.provider_error(operation, format!("unexpected answer: {error}")))
        })
    }
}

/// Longest permission list kept from a provider header.
const MAX_PERMISSION_CHARS: usize = 200;

/// The permissions GitHub says a refused request needs, from
/// `X-Accepted-GitHub-Permissions` (`issues=write`, or alternatives
/// separated by `;`). Only the characters such a list uses are kept, and
/// it is bounded, so the header cannot carry arbitrary text into an error.
fn accepted_permissions(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let value = headers
        .get("x-accepted-github-permissions")?
        .to_str()
        .ok()?;
    let value: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '=' | ',' | ';' | ' '))
        .take(MAX_PERMISSION_CHARS)
        .collect();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(format!("answer larger than {MAX_RESPONSE_BYTES} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Deserialize)]
struct GhIssue {
    number: i64,
    html_url: String,
    title: String,
    state: String,
}

#[derive(Deserialize)]
struct GlIssue {
    iid: i64,
    web_url: String,
    title: String,
    state: String,
}

#[derive(Deserialize)]
struct GhCommitAuthor {
    name: Option<String>,
    date: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct GhCommitDetail {
    message: String,
    author: Option<GhCommitAuthor>,
}

#[derive(Deserialize)]
struct GhCommit {
    sha: String,
    html_url: String,
    commit: GhCommitDetail,
}

#[derive(Deserialize)]
struct GlCommit {
    id: String,
    title: String,
    author_name: Option<String>,
    authored_date: Option<DateTime<Utc>>,
    web_url: String,
}

fn issue_state(state: &str) -> IssueState {
    // GitHub: open/closed. GitLab: opened/closed (and locked, which is open).
    if state == "closed" {
        IssueState::Closed
    } else {
        IssueState::Open
    }
}

fn first_line(message: &str) -> String {
    message
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

async fn create_issue(
    http: &reqwest::Client,
    api: &Api,
    title: &str,
    body: &str,
) -> Result<RepositoryIssue, SourceControlError> {
    let base = api.api_base.trim_end_matches('/');
    match api.flavor {
        Flavor::GitHub => {
            let url = format!("{base}/repos/{}/{}/issues", api.owner, api.repo);
            let request = api
                .request(http, reqwest::Method::POST, &url)
                .json(&serde_json::json!({ "title": title, "body": body }));
            let issue: GhIssue = api.send(request, "create an issue", "repository").await?;
            Ok(RepositoryIssue {
                number: issue.number,
                url: issue.html_url,
                title: issue.title,
                state: issue_state(&issue.state),
            })
        }
        Flavor::GitLab => {
            let url = format!("{base}/api/v4/projects/{}/issues", api.gitlab_project());
            let request = api
                .request(http, reqwest::Method::POST, &url)
                .json(&serde_json::json!({ "title": title, "description": body }));
            let issue: GlIssue = api.send(request, "create an issue", "repository").await?;
            Ok(RepositoryIssue {
                number: issue.iid,
                url: issue.web_url,
                title: issue.title,
                state: issue_state(&issue.state),
            })
        }
    }
}

async fn read_issue(
    http: &reqwest::Client,
    api: &Api,
    number: i64,
) -> Result<RepositoryIssue, SourceControlError> {
    let base = api.api_base.trim_end_matches('/');
    let what = format!("issue #{number}");
    match api.flavor {
        Flavor::GitHub => {
            let url = format!("{base}/repos/{}/{}/issues/{number}", api.owner, api.repo);
            let request = api.request(http, reqwest::Method::GET, &url);
            let issue: GhIssue = api.send_read(request, "read an issue", &what).await?;
            Ok(RepositoryIssue {
                number: issue.number,
                url: issue.html_url,
                title: issue.title,
                state: issue_state(&issue.state),
            })
        }
        Flavor::GitLab => {
            let url = format!(
                "{base}/api/v4/projects/{}/issues/{number}",
                api.gitlab_project()
            );
            let request = api.request(http, reqwest::Method::GET, &url);
            let issue: GlIssue = api.send_read(request, "read an issue", &what).await?;
            Ok(RepositoryIssue {
                number: issue.iid,
                url: issue.web_url,
                title: issue.title,
                state: issue_state(&issue.state),
            })
        }
    }
}

async fn list_commits(
    http: &reqwest::Client,
    api: &Api,
    path: &str,
    at: Option<&str>,
    limit: usize,
) -> Result<Vec<RepositoryCommit>, SourceControlError> {
    let base = api.api_base.trim_end_matches('/');
    let limit = limit.clamp(1, MAX_COMMITS).to_string();
    let what = match at {
        Some(at) => format!("commit {at} or path {path}"),
        None => format!("path {path}"),
    };
    match api.flavor {
        Flavor::GitHub => {
            let url = format!("{base}/repos/{}/{}/commits", api.owner, api.repo);
            let mut query = vec![("path", path), ("per_page", limit.as_str())];
            if let Some(at) = at {
                query.push(("sha", at));
            }
            let request = api.request(http, reqwest::Method::GET, &url).query(&query);
            let commits: Vec<GhCommit> = api.send_read(request, "list commits", &what).await?;
            Ok(commits
                .into_iter()
                .map(|c| RepositoryCommit {
                    summary: first_line(&c.commit.message),
                    author_name: c.commit.author.as_ref().and_then(|a| a.name.clone()),
                    authored_at: c.commit.author.and_then(|a| a.date),
                    sha: c.sha,
                    url: c.html_url,
                })
                .collect())
        }
        Flavor::GitLab => {
            let url = format!(
                "{base}/api/v4/projects/{}/repository/commits",
                api.gitlab_project()
            );
            let mut query = vec![("path", path), ("per_page", limit.as_str())];
            if let Some(at) = at {
                query.push(("ref_name", at));
            }
            let request = api.request(http, reqwest::Method::GET, &url).query(&query);
            let commits: Vec<GlCommit> = api.send_read(request, "list commits", &what).await?;
            Ok(commits
                .into_iter()
                .map(|c| RepositoryCommit {
                    summary: first_line(&c.title),
                    author_name: c.author_name,
                    authored_at: c.authored_date,
                    sha: c.id,
                    url: c.web_url,
                })
                .collect())
        }
    }
}

/// The repository's page, from the provider's web URL.
fn web_url(provider_type: &str, base_url: Option<&str>, owner: &str, repo: &str) -> String {
    let base = match (provider_type, base_url) {
        (_, Some(base)) if !base.trim().is_empty() => base.trim_end_matches('/').to_string(),
        ("github", _) => "https://github.com".to_string(),
        ("gitlab", _) => "https://gitlab.com".to_string(),
        ("bitbucket", _) => "https://bitbucket.org".to_string(),
        _ => String::new(),
    };
    format!("{base}/{owner}/{repo}")
}

/// [`ProjectSourceControl`] over the project's git provider connection.
pub struct GitSourceControl {
    db: Arc<DatabaseConnection>,
    manager: Arc<dyn GitProviderManagerTrait>,
    /// `Err` holds why the client could not be built; every call then
    /// fails with it.
    http: Result<reqwest::Client, String>,
}

impl GitSourceControl {
    pub fn new(db: Arc<DatabaseConnection>, manager: Arc<dyn GitProviderManagerTrait>) -> Self {
        let http = guarded_client_builder()
            .timeout(Duration::from_secs(10))
            .user_agent("Temps-Error-Tracking/1.0")
            .build()
            .map_err(|error| {
                warn!(%error, "Failed to build the source control HTTP client; issue links and suspect commits are unavailable");
                error.to_string()
            });
        Self { db, manager, http }
    }

    fn lookup_error(project_id: i32, reason: impl ToString) -> SourceControlError {
        SourceControlError::Lookup {
            project_id,
            reason: reason.to_string(),
        }
    }

    /// The project's repository, its provider row and connection id.
    async fn load(
        &self,
        project_id: i32,
    ) -> Result<
        Option<(ProjectRepository, temps_entities::git_providers::Model, i32)>,
        SourceControlError,
    > {
        use temps_entities::{git_provider_connections, git_providers, projects};
        let db = self.db.as_ref();
        let Some(project) = projects::Entity::find_by_id(project_id)
            .one(db)
            .await
            .map_err(|e| Self::lookup_error(project_id, e))?
        else {
            return Err(Self::lookup_error(project_id, "the project does not exist"));
        };
        let Some(connection_id) = project.git_provider_connection_id else {
            return Ok(None);
        };
        if project.repo_owner.is_empty() || project.repo_name.is_empty() {
            return Ok(None);
        }
        let connection = git_provider_connections::Entity::find_by_id(connection_id)
            .one(db)
            .await
            .map_err(|e| Self::lookup_error(project_id, e))?
            .ok_or_else(|| {
                Self::lookup_error(
                    project_id,
                    format!("git provider connection {connection_id} does not exist"),
                )
            })?;
        let provider = git_providers::Entity::find_by_id(connection.provider_id)
            .one(db)
            .await
            .map_err(|e| Self::lookup_error(project_id, e))?
            .ok_or_else(|| {
                Self::lookup_error(
                    project_id,
                    format!("git provider {} does not exist", connection.provider_id),
                )
            })?;
        let repository = ProjectRepository {
            provider: provider.provider_type.clone(),
            repository: format!("{}/{}", project.repo_owner, project.repo_name),
            web_url: web_url(
                &provider.provider_type,
                provider.base_url.as_deref(),
                &project.repo_owner,
                &project.repo_name,
            ),
            default_branch: project.main_branch.clone(),
        };
        Ok(Some((repository, provider, connection_id)))
    }

    /// The API of the project's repository, with a fresh token.
    async fn api(&self, project_id: i32) -> Result<(Api, &reqwest::Client), SourceControlError> {
        let http = self
            .http
            .as_ref()
            .map_err(|reason| Self::lookup_error(project_id, reason))?;
        let (repository, provider, connection_id) = self
            .load(project_id)
            .await?
            .ok_or(SourceControlError::NotConnected { project_id })?;
        let (flavor, api_base) = match provider.provider_type.as_str() {
            "github" => (
                Flavor::GitHub,
                provider
                    .api_url
                    .clone()
                    .unwrap_or_else(|| "https://api.github.com".to_string()),
            ),
            "gitlab" => (
                Flavor::GitLab,
                gitlab_root(provider.api_url.as_deref(), provider.base_url.as_deref()),
            ),
            other => {
                return Err(SourceControlError::Unsupported {
                    project_id,
                    provider: other.to_string(),
                })
            }
        };
        let (token, _) = self
            .manager
            .get_connection_access_token(connection_id)
            .await
            .map_err(|e| Self::lookup_error(project_id, e))?;
        let (owner, repo) = repository
            .repository
            .split_once('/')
            .map(|(o, r)| (o.to_string(), r.to_string()))
            .unwrap_or_default();
        Ok((
            Api {
                project_id,
                flavor,
                api_base,
                token,
                owner,
                repo,
                retry: RetryConfig::new(READ_ATTEMPTS)
                    .with_base_delay(Duration::from_millis(500))
                    .with_max_delay(Duration::from_secs(2)),
            },
            http,
        ))
    }
}

#[async_trait]
impl ProjectSourceControl for GitSourceControl {
    async fn repository(
        &self,
        project_id: i32,
    ) -> Result<Option<ProjectRepository>, SourceControlError> {
        Ok(self
            .load(project_id)
            .await?
            .map(|(repository, _, _)| repository))
    }

    async fn create_issue(
        &self,
        project_id: i32,
        title: &str,
        body: &str,
    ) -> Result<RepositoryIssue, SourceControlError> {
        let (api, http) = self.api(project_id).await?;
        create_issue(http, &api, title, body).await
    }

    async fn issue(
        &self,
        project_id: i32,
        number: i64,
    ) -> Result<RepositoryIssue, SourceControlError> {
        let (api, http) = self.api(project_id).await?;
        read_issue(http, &api, number).await
    }

    async fn commits_touching(
        &self,
        project_id: i32,
        path: &str,
        at: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RepositoryCommit>, SourceControlError> {
        let (api, http) = self.api(project_id).await?;
        list_commits(http, &api, path, at, limit).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::{Matcher, Server};

    fn api(flavor: Flavor, base: &str) -> Api {
        Api {
            project_id: 4,
            flavor,
            api_base: base.to_string(),
            token: "tok".into(),
            owner: "acme".into(),
            repo: "shop".into(),
            retry: RetryConfig::new(READ_ATTEMPTS).with_base_delay(Duration::from_millis(1)),
        }
    }

    #[tokio::test]
    async fn github_opens_an_issue_and_reads_its_state() {
        let mut server = Server::new_async().await;
        let create = server
            .mock("POST", "/repos/acme/shop/issues")
            .match_header("authorization", "Bearer tok")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "title": "TypeError: x is undefined",
                "body": "details"
            })))
            .with_status(201)
            .with_body(r#"{"number":12,"html_url":"https://github.com/acme/shop/issues/12","title":"TypeError: x is undefined","state":"open"}"#)
            .create_async()
            .await;
        let read = server
            .mock("GET", "/repos/acme/shop/issues/12")
            .with_status(200)
            .with_body(r#"{"number":12,"html_url":"https://github.com/acme/shop/issues/12","title":"TypeError: x is undefined","state":"closed"}"#)
            .create_async()
            .await;
        let http = reqwest::Client::new();
        let api = api(Flavor::GitHub, &server.url());
        let issue = create_issue(&http, &api, "TypeError: x is undefined", "details")
            .await
            .expect("created");
        assert_eq!(issue.number, 12);
        assert_eq!(issue.state, IssueState::Open);
        let issue = read_issue(&http, &api, 12).await.expect("read");
        assert_eq!(issue.state, IssueState::Closed);
        create.assert_async().await;
        read.assert_async().await;
    }

    #[tokio::test]
    async fn gitlab_opens_an_issue_with_a_description_in_the_encoded_project() {
        let mut server = Server::new_async().await;
        let create = server
            .mock("POST", "/api/v4/projects/acme%2Fshop/issues")
            .match_header("private-token", "tok")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "title": "Boom",
                "description": "details"
            })))
            .with_status(201)
            .with_body(r#"{"iid":3,"web_url":"https://gitlab.example/acme/shop/-/issues/3","title":"Boom","state":"opened"}"#)
            .create_async()
            .await;
        let http = reqwest::Client::new();
        let issue = create_issue(
            &http,
            &api(Flavor::GitLab, &server.url()),
            "Boom",
            "details",
        )
        .await
        .expect("created");
        assert_eq!(issue.number, 3);
        assert_eq!(issue.state, IssueState::Open);
        assert!(issue.url.ends_with("/-/issues/3"));
        create.assert_async().await;
    }

    #[tokio::test]
    async fn a_refused_token_is_forbidden_and_a_missing_issue_names_its_number() {
        let mut server = Server::new_async().await;
        let _create = server
            .mock("POST", "/repos/acme/shop/issues")
            .with_status(403)
            .create_async()
            .await;
        let _read = server
            .mock("GET", "/repos/acme/shop/issues/99")
            .with_status(404)
            .create_async()
            .await;
        let http = reqwest::Client::new();
        let api = api(Flavor::GitHub, &server.url());
        let error = create_issue(&http, &api, "t", "b").await.expect_err("403");
        assert!(matches!(
            error,
            SourceControlError::Forbidden {
                status: 403,
                project_id: 4,
                ..
            }
        ));
        let error = read_issue(&http, &api, 99).await.expect_err("404");
        assert!(error.to_string().contains("issue #99"), "{error}");
        assert!(error.to_string().contains("acme/shop"), "{error}");
    }

    #[tokio::test]
    async fn an_installation_without_the_issues_permission_is_told_what_to_grant() {
        let mut server = Server::new_async().await;
        let _create = server
            .mock("POST", "/repos/acme/shop/issues")
            .with_status(403)
            .with_header("x-accepted-github-permissions", "issues=write")
            .with_body(r#"{"message":"Resource not accessible by integration"}"#)
            .create_async()
            .await;
        let http = reqwest::Client::new();
        let api = api(Flavor::GitHub, &server.url());
        let error = create_issue(&http, &api, "t", "b").await.expect_err("403");
        match &error {
            SourceControlError::MissingPermission {
                project_id,
                permission,
                operation,
                ..
            } => {
                assert_eq!(*project_id, 4);
                assert_eq!(permission, "issues=write");
                assert_eq!(*operation, "create an issue");
            }
            other => panic!("expected a missing permission, got {other:?}"),
        }
        let message = error.to_string();
        assert!(message.contains("issues=write"), "{message}");
        assert!(message.contains("acme/shop"), "{message}");
        assert!(message.contains("installation"), "{message}");
    }

    #[test]
    fn a_permission_header_is_bounded_and_keeps_only_permission_text() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(accepted_permissions(&headers), None);
        headers.insert(
            "x-accepted-github-permissions",
            reqwest::header::HeaderValue::from_static("issues=write; pull_requests=write<script>"),
        );
        assert_eq!(
            accepted_permissions(&headers).as_deref(),
            Some("issues=write; pull_requests=writescript")
        );
        let long = format!("issues={}", "w".repeat(1000));
        headers.insert(
            "x-accepted-github-permissions",
            reqwest::header::HeaderValue::from_str(&long).expect("header"),
        );
        assert_eq!(
            accepted_permissions(&headers).map(|p| p.chars().count()),
            Some(MAX_PERMISSION_CHARS)
        );
        headers.insert(
            "x-accepted-github-permissions",
            reqwest::header::HeaderValue::from_static("  "),
        );
        assert_eq!(accepted_permissions(&headers), None);
    }

    #[tokio::test]
    async fn commits_are_read_by_path_at_a_commit_on_both_providers() {
        let mut server = Server::new_async().await;
        let github = server
            .mock("GET", "/repos/acme/shop/commits")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("path".into(), "src/cart.ts".into()),
                Matcher::UrlEncoded("sha".into(), "abc123".into()),
                Matcher::UrlEncoded("per_page".into(), "5".into()),
            ]))
            .with_status(200)
            .with_body(r#"[{"sha":"abc123","html_url":"https://github.com/acme/shop/commit/abc123","commit":{"message":"Fix the cart total\n\nLonger text","author":{"name":"Sam Doe","date":"2026-11-01T10:00:00Z"}}}]"#)
            .create_async()
            .await;
        let gitlab = server
            .mock("GET", "/api/v4/projects/acme%2Fshop/repository/commits")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("path".into(), "src/cart.ts".into()),
                Matcher::UrlEncoded("ref_name".into(), "main".into()),
            ]))
            .with_status(200)
            .with_body(r#"[{"id":"def456","title":"Touch the cart","author_name":"Alex Roe","authored_date":"2026-10-30T09:00:00Z","web_url":"https://gitlab.example/acme/shop/-/commit/def456"}]"#)
            .create_async()
            .await;
        let http = reqwest::Client::new();
        let commits = list_commits(
            &http,
            &api(Flavor::GitHub, &server.url()),
            "src/cart.ts",
            Some("abc123"),
            5,
        )
        .await
        .expect("github commits");
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].summary, "Fix the cart total");
        assert_eq!(commits[0].author_name.as_deref(), Some("Sam Doe"));
        let commits = list_commits(
            &http,
            &api(Flavor::GitLab, &server.url()),
            "src/cart.ts",
            Some("main"),
            50,
        )
        .await
        .expect("gitlab commits");
        assert_eq!(commits[0].sha, "def456");
        github.assert_async().await;
        gitlab.assert_async().await;
    }

    #[tokio::test]
    async fn an_unknown_commit_is_not_found_and_an_oversized_answer_is_refused() {
        let mut server = Server::new_async().await;
        let _unknown = server
            .mock("GET", "/repos/acme/shop/commits")
            .match_query(Matcher::Any)
            .with_status(422)
            .create_async()
            .await;
        let http = reqwest::Client::new();
        let error = list_commits(
            &http,
            &api(Flavor::GitHub, &server.url()),
            "a.rs",
            Some("zzz"),
            5,
        )
        .await
        .expect_err("unknown commit");
        assert!(
            matches!(error, SourceControlError::NotFound { .. }),
            "{error}"
        );

        let mut server = Server::new_async().await;
        let huge = format!("[{}]", "\"x\",".repeat(MAX_RESPONSE_BYTES / 4) + "\"x\"");
        let _big = server
            .mock("GET", "/repos/acme/shop/issues/1")
            .with_status(200)
            .with_body(huge)
            .create_async()
            .await;
        let error = read_issue(&http, &api(Flavor::GitHub, &server.url()), 1)
            .await
            .expect_err("too large");
        assert!(error.to_string().contains("larger than"), "{error}");
    }

    #[test]
    fn a_gitlab_api_url_stored_with_or_without_api_v4_reaches_the_same_root() {
        for stored in [
            "https://gitlab.example.com/api/v4",
            "https://gitlab.example.com/api/v4/",
            "https://gitlab.example.com",
            "https://gitlab.example.com/",
        ] {
            assert_eq!(
                gitlab_root(Some(stored), None),
                "https://gitlab.example.com",
                "{stored}"
            );
        }
        assert_eq!(
            gitlab_root(None, Some("https://git.example.org/")),
            "https://git.example.org"
        );
        assert_eq!(gitlab_root(Some(" "), None), "https://gitlab.com");
        assert_eq!(gitlab_root(None, None), "https://gitlab.com");
    }

    #[tokio::test]
    async fn a_gitlab_api_url_with_api_v4_reads_an_issue_without_doubling_it() {
        let mut server = Server::new_async().await;
        let read = server
            .mock("GET", "/api/v4/projects/acme%2Fshop/issues/3")
            .with_status(200)
            .with_body(r#"{"iid":3,"web_url":"https://gitlab.example/acme/shop/-/issues/3","title":"Boom","state":"opened"}"#)
            .create_async()
            .await;
        let root = gitlab_root(Some(&format!("{}/api/v4", server.url())), None);
        let issue = read_issue(&reqwest::Client::new(), &api(Flavor::GitLab, &root), 3)
            .await
            .expect("read");
        assert_eq!(issue.number, 3);
        read.assert_async().await;
    }

    #[tokio::test]
    async fn a_read_is_retried_after_a_server_error_and_a_write_is_not() {
        let mut server = Server::new_async().await;
        // A mock still owed hits answers first: one 502, then the issue.
        let failing = server
            .mock("GET", "/repos/acme/shop/issues/5")
            .with_status(502)
            .expect(1)
            .create_async()
            .await;
        let ok = server
            .mock("GET", "/repos/acme/shop/issues/5")
            .with_status(200)
            .with_body(r#"{"number":5,"html_url":"https://github.com/acme/shop/issues/5","title":"t","state":"open"}"#)
            .expect(1)
            .create_async()
            .await;
        let http = reqwest::Client::new();
        let api = api(Flavor::GitHub, &server.url());
        let issue = read_issue(&http, &api, 5).await.expect("read on the retry");
        assert_eq!(issue.number, 5);
        failing.assert_async().await;
        ok.assert_async().await;

        let create = server
            .mock("POST", "/repos/acme/shop/issues")
            .with_status(503)
            .expect(1)
            .create_async()
            .await;
        let error = create_issue(&http, &api, "t", "b")
            .await
            .expect_err("a failed create is not retried");
        assert!(
            matches!(error, SourceControlError::Provider { .. }),
            "{error:?}"
        );
        create.assert_async().await;
    }

    #[tokio::test]
    async fn a_refused_or_missing_read_is_not_retried() {
        let mut server = Server::new_async().await;
        let refused = server
            .mock("GET", "/repos/acme/shop/issues/1")
            .with_status(401)
            .expect(1)
            .create_async()
            .await;
        let missing = server
            .mock("GET", "/repos/acme/shop/issues/2")
            .with_status(404)
            .expect(1)
            .create_async()
            .await;
        let http = reqwest::Client::new();
        let api = api(Flavor::GitHub, &server.url());
        assert!(matches!(
            read_issue(&http, &api, 1).await,
            Err(SourceControlError::Forbidden { .. })
        ));
        assert!(matches!(
            read_issue(&http, &api, 2).await,
            Err(SourceControlError::NotFound { .. })
        ));
        refused.assert_async().await;
        missing.assert_async().await;
    }

    #[test]
    fn the_repository_page_uses_the_provider_web_url_or_the_public_host() {
        assert_eq!(
            web_url("github", None, "acme", "shop"),
            "https://github.com/acme/shop"
        );
        assert_eq!(
            web_url("gitlab", Some("https://git.example.test/"), "acme", "shop"),
            "https://git.example.test/acme/shop"
        );
        assert_eq!(first_line("one\ntwo"), "one");
        assert_eq!(issue_state("opened"), IssueState::Open);
        assert_eq!(issue_state("closed"), IssueState::Closed);
    }
}
