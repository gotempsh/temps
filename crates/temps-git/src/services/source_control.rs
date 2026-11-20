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

    /// Send the request and decode a successful JSON answer. `what` names
    /// the thing a 404 is missing.
    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        operation: &'static str,
        what: &str,
    ) -> Result<T, SourceControlError> {
        let response = request
            .send()
            .await
            .map_err(|error| self.provider_error(operation, error.to_string()))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(SourceControlError::Forbidden {
                project_id: self.project_id,
                provider: self.flavor.name().into(),
                repository: self.repository(),
                operation,
                status: status.as_u16(),
            });
        }
        // GitHub answers 422 for a commit it does not know.
        if status == reqwest::StatusCode::NOT_FOUND
            || (status == reqwest::StatusCode::UNPROCESSABLE_ENTITY && operation == "list commits")
        {
            return Err(SourceControlError::NotFound {
                project_id: self.project_id,
                provider: self.flavor.name().into(),
                repository: self.repository(),
                what: what.to_string(),
            });
        }
        if !status.is_success() {
            return Err(self.provider_error(operation, format!("HTTP {status}")));
        }
        let body = read_capped(response)
            .await
            .map_err(|reason| self.provider_error(operation, reason))?;
        serde_json::from_slice(&body)
            .map_err(|error| self.provider_error(operation, format!("unexpected answer: {error}")))
    }
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
            let issue: GhIssue = api.send(request, "read an issue", &what).await?;
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
            let issue: GlIssue = api.send(request, "read an issue", &what).await?;
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
            let commits: Vec<GhCommit> = api.send(request, "list commits", &what).await?;
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
            let commits: Vec<GlCommit> = api.send(request, "list commits", &what).await?;
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
                provider
                    .api_url
                    .clone()
                    .or_else(|| provider.base_url.clone())
                    .unwrap_or_else(|| "https://gitlab.com".to_string()),
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
