use super::git_provider::{
    AuthMethod, Branch, Commit, FileContent, GitProviderError, GitProviderService, GitProviderTag,
    GitProviderType, PullRequest, Repository, RepositoryPage, User, WebhookConfig,
};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

// Response structs for API calls

/// Token response from GitLab OAuth and App flows
/// GitLab always returns refresh_token in OAuth responses
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct HookResponse {
    id: i64,
}

#[derive(Deserialize)]
struct GitLabCommitResponse {
    id: String,
    #[allow(dead_code)]
    short_id: String,
    #[allow(dead_code)]
    title: String,
    message: String,
    author_name: String,
    author_email: String,
    #[allow(dead_code)]
    authored_date: String,
    #[allow(dead_code)]
    committer_name: String,
    #[allow(dead_code)]
    committer_email: String,
    committed_date: String,
    #[allow(dead_code)]
    web_url: String,
}

pub struct GitLabProvider {
    base_url: String,
    auth_method: AuthMethod,
}

impl GitLabProvider {
    pub fn new(base_url: Option<String>, auth_method: AuthMethod) -> Self {
        Self {
            base_url: base_url.unwrap_or_else(|| "https://gitlab.com".to_string()),
            auth_method,
        }
    }

    fn get_client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .user_agent("Temps-Engine/1.0")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to build reqwest client with static config")
    }

    /// Retry configuration for GitLab API calls.
    fn retry_config() -> temps_core::retry::RetryConfig {
        temps_core::retry::RetryConfig::new(3)
            .with_base_delay(std::time::Duration::from_secs(1))
            .with_max_delay(std::time::Duration::from_secs(10))
    }

    /// Send an HTTP request with retry logic for transient failures.
    async fn send_with_retry<F>(
        &self,
        mut build_request: F,
    ) -> Result<reqwest::Response, GitProviderError>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        Self::retry_config()
            .retry(|| {
                let request = build_request();
                async move {
                    let response = request
                        .send()
                        .await
                        .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

                    let status = response.status();
                    if status.is_server_error() || status.as_u16() == 429 {
                        let error_text = response.text().await.unwrap_or_default();
                        return Err(GitProviderError::ApiError(format!(
                            "HTTP {}: {}",
                            status, error_text
                        )));
                    }

                    Ok(response)
                }
            })
            .await
    }

    fn get_headers(&self, access_token: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();

        // Use different header based on auth method
        match &self.auth_method {
            AuthMethod::PersonalAccessToken { .. } => {
                // PAT uses PRIVATE-TOKEN header
                headers.insert(
                    "PRIVATE-TOKEN",
                    reqwest::header::HeaderValue::from_str(access_token).unwrap(),
                );
            }
            AuthMethod::GitLabApp { .. } | AuthMethod::OAuth { .. } => {
                // OAuth/GitLab App uses Bearer token
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    reqwest::header::HeaderValue::from_str(&format!("Bearer {}", access_token))
                        .unwrap(),
                );
            }
            _ => {
                // Default to Bearer token for other methods
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    reqwest::header::HeaderValue::from_str(&format!("Bearer {}", access_token))
                        .unwrap(),
                );
            }
        }

        headers
    }

    /// Refresh an access token using a refresh token
    async fn refresh_access_token(
        &self,
        client_id: &str,
        client_secret: &str,
        refresh_token: &str,
    ) -> Result<(String, Option<String>), GitProviderError> {
        info!("Refreshing GitLab access token");

        let client = self.get_client();
        let params = [
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ];

        let url = format!("{}/oauth/token", self.base_url);
        let params_owned: Vec<(String, String)> = params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let response = self
            .send_with_retry(|| client.post(&url).form(&params_owned))
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to refresh token: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(GitProviderError::AuthenticationFailed(format!(
                "Failed to refresh token: {} - {}",
                status, error_text
            )));
        }

        let token_response: TokenResponse = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse refresh response: {}", e))
        })?;

        debug!("Successfully refreshed GitLab access token");
        Ok((token_response.access_token, token_response.refresh_token))
    }

    /// Validate a GitLab access token by making a simple API call.
    /// Note: This method does NOT use send_with_retry because 401/403/429
    /// responses are meaningful status codes, not transient errors.
    async fn validate_token_internal(&self, access_token: &str) -> Result<bool, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        // Use the /user endpoint to validate the token
        let url = format!("{}/api/v4/user", self.base_url);
        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await;

        // send_with_retry returns Err for 5xx/429 — handle the retry-level error
        let response = match response {
            Ok(resp) => resp,
            Err(_) => return Err(GitProviderError::RateLimitExceeded),
        };

        // Token is valid if we get a 200 OK
        // 401 means unauthorized (invalid token)
        match response.status() {
            status if status.is_success() => Ok(true),
            status if status.as_u16() == 401 => Ok(false),
            status if status.as_u16() == 403 => Ok(false),
            status => {
                let error_text = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "Unknown error".to_string());
                Err(GitProviderError::ApiError(format!(
                    "Unexpected response validating token: {} - {}",
                    status, error_text
                )))
            }
        }
    }
}

#[async_trait]
impl GitProviderService for GitLabProvider {
    fn provider_type(&self) -> GitProviderType {
        GitProviderType::GitLab
    }

    async fn authenticate(&self, code: Option<String>) -> Result<String, GitProviderError> {
        match &self.auth_method {
            AuthMethod::GitLabApp {
                app_id,
                app_secret,
                redirect_uri,
            } => {
                if let Some(code) = code {
                    // Exchange authorization code for access token
                    let client = self.get_client();
                    let params_owned = vec![
                        ("client_id".to_string(), app_id.clone()),
                        ("client_secret".to_string(), app_secret.clone()),
                        ("code".to_string(), code),
                        ("grant_type".to_string(), "authorization_code".to_string()),
                        ("redirect_uri".to_string(), redirect_uri.clone()),
                    ];

                    let url = format!("{}/oauth/token", self.base_url);
                    let response = self
                        .send_with_retry(|| client.post(&url).form(&params_owned))
                        .await
                        .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

                    let token_response: TokenResponse = response
                        .json()
                        .await
                        .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

                    // Note: GitLab returns both access_token and refresh_token
                    // The caller should store the refresh_token for later use
                    Ok(token_response.access_token)
                } else {
                    Err(GitProviderError::AuthenticationFailed(
                        "Authorization code required".to_string(),
                    ))
                }
            }
            AuthMethod::OAuth {
                client_id,
                client_secret,
                redirect_uri,
            } => {
                if let Some(code) = code {
                    // Exchange authorization code for access token
                    let client = self.get_client();
                    let params_owned = vec![
                        ("client_id".to_string(), client_id.clone()),
                        ("client_secret".to_string(), client_secret.clone()),
                        ("code".to_string(), code),
                        ("grant_type".to_string(), "authorization_code".to_string()),
                        ("redirect_uri".to_string(), redirect_uri.clone()),
                    ];

                    let url = format!("{}/oauth/token", self.base_url);
                    let response = self
                        .send_with_retry(|| client.post(&url).form(&params_owned))
                        .await
                        .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

                    let token_response: TokenResponse = response
                        .json()
                        .await
                        .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

                    Ok(token_response.access_token)
                } else {
                    Err(GitProviderError::AuthenticationFailed(
                        "Authorization code required".to_string(),
                    ))
                }
            }
            AuthMethod::PersonalAccessToken { token } => {
                // PAT is already the access token
                Ok(token.clone())
            }
            _ => Err(GitProviderError::NotImplemented),
        }
    }

    async fn get_auth_url(&self, state: &str) -> Result<String, GitProviderError> {
        match &self.auth_method {
            AuthMethod::GitLabApp {
                app_id,
                redirect_uri,
                ..
            }
            | AuthMethod::OAuth {
                client_id: app_id,
                redirect_uri,
                ..
            } => {
                let auth_url = format!(
                    "{}/oauth/authorize?client_id={}&redirect_uri={}&response_type=code&state={}&scope=api+read_user+read_repository",
                    self.base_url, app_id, redirect_uri, state
                );
                Ok(auth_url)
            }
            _ => Err(GitProviderError::NotImplemented),
        }
    }

    async fn token_needs_refresh(&self, access_token: &str) -> bool {
        // Check if the token is valid by making a simple API call
        match self.validate_token_internal(access_token).await {
            Ok(true) => false, // Token is valid, no refresh needed
            Ok(false) => true, // Token is invalid, needs refresh
            Err(_) => true,    // Error validating, assume it needs refresh
        }
    }

    async fn validate_token(&self, access_token: &str) -> Result<bool, GitProviderError> {
        self.validate_token_internal(access_token).await
    }

    async fn validate_and_refresh_token(
        &self,
        access_token: &str,
        refresh_token: Option<&str>,
    ) -> Result<(String, Option<String>), GitProviderError> {
        // First, validate the current token
        match self.validate_token_internal(access_token).await {
            Ok(true) => {
                // Token is valid, return it as-is
                debug!("GitLab access token is still valid");
                Ok((access_token.to_string(), None))
            }
            Ok(false) | Err(GitProviderError::RateLimitExceeded) => {
                // Token is invalid or expired, try to refresh if we have a refresh token
                if let Some(refresh_token) = refresh_token {
                    info!("GitLab access token is invalid or expired, attempting refresh");

                    // Get credentials based on auth method
                    match &self.auth_method {
                        AuthMethod::GitLabApp {
                            app_id, app_secret, ..
                        } => {
                            let (new_access_token, new_refresh_token) = self
                                .refresh_access_token(app_id, app_secret, refresh_token)
                                .await?;
                            Ok((new_access_token, new_refresh_token))
                        }
                        AuthMethod::OAuth {
                            client_id,
                            client_secret,
                            ..
                        } => {
                            let (new_access_token, new_refresh_token) = self
                                .refresh_access_token(client_id, client_secret, refresh_token)
                                .await?;
                            Ok((new_access_token, new_refresh_token))
                        }
                        AuthMethod::PersonalAccessToken { .. } => {
                            // PATs don't support refresh
                            debug!("Personal Access Token cannot be refreshed");
                            Err(GitProviderError::AuthenticationFailed(
                                "Personal Access Token is invalid and cannot be refreshed"
                                    .to_string(),
                            ))
                        }
                        _ => Err(GitProviderError::NotImplemented),
                    }
                } else {
                    // No refresh token available
                    Err(GitProviderError::AuthenticationFailed(
                        "Access token is invalid and no refresh token is available".to_string(),
                    ))
                }
            }
            Err(e) => {
                // Some other error occurred during validation
                error!("Error validating GitLab token: {}", e);
                Err(e)
            }
        }
    }

    async fn list_repositories(
        &self,
        access_token: &str,
        organization: Option<&str>,
    ) -> Result<Vec<Repository>, GitProviderError> {
        // Thin wrapper around `list_repositories_page`. Kept for callers that
        // don't care about streaming (e.g. `list_repositories_by_connection`
        // which just wants a snapshot). Large syncs should use the paged API
        // directly so they can flush per page.
        const MAX_PAGES: u32 = 200;
        let mut all = Vec::new();
        let mut page: u32 = 1;
        loop {
            let RepositoryPage { items, next_page } = self
                .list_repositories_page(access_token, organization, page)
                .await?;
            all.extend(items);
            match next_page {
                Some(next) if next > page && page < MAX_PAGES => page = next,
                _ => break,
            }
        }
        Ok(all)
    }

    async fn list_repositories_page(
        &self,
        access_token: &str,
        organization: Option<&str>,
        page: u32,
    ) -> Result<RepositoryPage, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        // GitLab caps per_page at 100. The silent default of 20 was the reason
        // nested-group users lost repos past the first page.
        const PER_PAGE: u32 = 100;
        let page = page.max(1);

        // Group paths can include slashes (nested groups like `foo/bar`).
        // GitLab's REST API requires them to be URL-encoded as a single path
        // segment. include_subgroups=true: without this, projects in
        // descendant groups are not returned. archived=false skips stale
        // archived projects. The /projects?membership=true endpoint doesn't
        // accept include_subgroups — membership already spans subgroups.
        let base_url = if let Some(org) = organization {
            let encoded = urlencoding::encode(org);
            format!(
                "{}/api/v4/groups/{}/projects?include_subgroups=true&archived=false",
                self.base_url, encoded
            )
        } else {
            format!(
                "{}/api/v4/projects?membership=true&archived=false",
                self.base_url
            )
        };
        let paged_url = format!("{}&per_page={}&page={}", base_url, PER_PAGE, page);

        let response = self
            .send_with_retry(|| client.get(&paged_url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to list repositories (page {}): {}",
                page,
                response.status()
            )));
        }

        // Capture pagination hints BEFORE consuming the body.
        let next_page_header = response
            .headers()
            .get("x-next-page")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u32>().ok());

        #[derive(Deserialize)]
        struct GitLabProject {
            id: i64,
            path: String,
            path_with_namespace: String,
            description: Option<String>,
            visibility: String,
            default_branch: Option<String>,
            http_url_to_repo: String,
            ssh_url_to_repo: String,
            web_url: String,
            star_count: i32,
            forks_count: i32,
            created_at: String,
            last_activity_at: String,
        }

        let projects: Vec<GitLabProject> = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        let received = projects.len();
        let items: Vec<Repository> = projects
            .into_iter()
            .map(|p| {
                // GitLab supports nested groups. `path_with_namespace` is the full
                // project path (e.g. "group/subgroup/repo-slug"); `path` is the
                // repo slug. Owner is everything before the last slash.
                let owner = p
                    .path_with_namespace
                    .rsplit_once('/')
                    .map(|(ns, _)| ns.to_string())
                    .unwrap_or_default();

                Repository {
                    id: p.id.to_string(),
                    name: p.path,
                    full_name: p.path_with_namespace,
                    owner,
                    description: p.description,
                    private: p.visibility != "public",
                    default_branch: p.default_branch.unwrap_or_else(|| "main".to_string()),
                    clone_url: p.http_url_to_repo,
                    ssh_url: p.ssh_url_to_repo,
                    web_url: p.web_url,
                    language: None,
                    size: 0,
                    stars: p.star_count,
                    forks: p.forks_count,
                    created_at: chrono::DateTime::parse_from_rfc3339(&p.created_at)
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .unwrap_or_else(|_| chrono::Utc::now()),
                    updated_at: chrono::DateTime::parse_from_rfc3339(&p.last_activity_at)
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .unwrap_or_else(|_| chrono::Utc::now()),
                    pushed_at: None,
                }
            })
            .collect();

        // Only advertise a next page when GitLab says so AND the current page
        // was full. If either signal is missing we're done — some self-hosted
        // instances omit the X-Next-Page header entirely.
        let next_page = match next_page_header {
            Some(next) if next > page && (received as u32) == PER_PAGE => Some(next),
            _ => None,
        };

        Ok(RepositoryPage { items, next_page })
    }

    async fn get_repository(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Repository, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        // In GitLab, we use the path with namespace
        let project_path = format!("{}/{}", owner, repo);
        let encoded_path = urlencoding::encode(&project_path);
        let url = format!("{}/api/v4/projects/{}", self.base_url, encoded_path);

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to get repository: {}",
                response.status()
            )));
        }

        #[derive(Deserialize)]
        struct GitLabProject {
            id: i64,
            path: String,
            path_with_namespace: String,
            description: Option<String>,
            visibility: String,
            default_branch: Option<String>,
            http_url_to_repo: String,
            ssh_url_to_repo: String,
            web_url: String,
            star_count: i32,
            forks_count: i32,
            created_at: String,
            last_activity_at: String,
        }

        let project: GitLabProject = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        // Recompute owner from path_with_namespace to handle nested groups,
        // since the caller's `owner` may be a stale or partial value.
        let owner = project
            .path_with_namespace
            .rsplit_once('/')
            .map(|(ns, _)| ns.to_string())
            .unwrap_or_else(|| owner.to_string());

        Ok(Repository {
            id: project.id.to_string(),
            name: project.path,
            full_name: project.path_with_namespace,
            owner,
            description: project.description,
            private: project.visibility != "public",
            default_branch: project.default_branch.unwrap_or_else(|| "main".to_string()),
            clone_url: project.http_url_to_repo,
            ssh_url: project.ssh_url_to_repo,
            web_url: project.web_url,
            language: None,
            size: 0,
            stars: project.star_count,
            forks: project.forks_count,
            created_at: chrono::DateTime::parse_from_rfc3339(&project.created_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
            updated_at: chrono::DateTime::parse_from_rfc3339(&project.last_activity_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
            pushed_at: None,
        })
    }

    async fn list_branches(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<Branch>, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let project_path = format!("{}/{}", owner, repo);
        let encoded_path = urlencoding::encode(&project_path);
        let url = format!(
            "{}/api/v4/projects/{}/repository/branches",
            self.base_url, encoded_path
        );

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to list branches: {}",
                response.status()
            )));
        }

        #[derive(Deserialize)]
        struct GitLabBranch {
            name: String,
            commit: GitLabCommit,
            protected: bool,
        }

        #[derive(Deserialize)]
        struct GitLabCommit {
            id: String,
        }

        let gitlab_branches: Vec<GitLabBranch> = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        let branches = gitlab_branches
            .into_iter()
            .map(|b| Branch {
                name: b.name,
                commit_sha: b.commit.id,
                protected: b.protected,
            })
            .collect();

        Ok(branches)
    }

    async fn list_tags(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<GitProviderTag>, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let project_path = format!("{}/{}", owner, repo);
        let encoded_path = urlencoding::encode(&project_path);

        let url = format!(
            "{}/api/v4/projects/{}/repository/tags",
            self.base_url, encoded_path
        );

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to list tags: {}",
                response.status()
            )));
        }

        #[derive(Deserialize)]
        struct GitLabTag {
            name: String,
            commit: GitLabCommitRef,
        }

        #[derive(Deserialize)]
        struct GitLabCommitRef {
            id: String,
        }

        let gitlab_tags: Vec<GitLabTag> = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse tags response: {}", e))
        })?;

        let tags = gitlab_tags
            .into_iter()
            .map(|t| GitProviderTag {
                name: t.name,
                commit_sha: t.commit.id,
            })
            .collect();

        Ok(tags)
    }

    async fn get_file_content(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        path: &str,
        branch: Option<&str>,
    ) -> Result<FileContent, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let project_path = format!("{}/{}", owner, repo);
        let encoded_path = urlencoding::encode(&project_path);
        let file_path = urlencoding::encode(path);

        let mut url = format!(
            "{}/api/v4/projects/{}/repository/files/{}",
            self.base_url, encoded_path, file_path
        );
        if let Some(ref_name) = branch {
            url.push_str(&format!("?ref={}", urlencoding::encode(ref_name)));
        }

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to get file content: {}",
                response.status()
            )));
        }

        #[derive(Deserialize)]
        struct GitLabFile {
            file_path: String,
            content: String,
            encoding: String,
        }

        let file: GitLabFile = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        Ok(FileContent {
            path: file.file_path,
            content: file.content,
            encoding: file.encoding,
        })
    }

    async fn create_webhook(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        config: WebhookConfig,
    ) -> Result<String, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let project_path = format!("{}/{}", owner, repo);
        let encoded_path = urlencoding::encode(&project_path);
        let url = format!("{}/api/v4/projects/{}/hooks", self.base_url, encoded_path);

        #[derive(Serialize)]
        struct CreateHookRequest {
            url: String,
            token: Option<String>,
            push_events: bool,
            merge_requests_events: bool,
            wiki_page_events: bool,
            tag_push_events: bool,
            issues_events: bool,
            note_events: bool,
            pipeline_events: bool,
        }

        let request = CreateHookRequest {
            url: config.url,
            token: config.secret,
            push_events: config.events.contains(&"push".to_string()),
            merge_requests_events: config.events.contains(&"merge_request".to_string()),
            wiki_page_events: false,
            tag_push_events: config.events.contains(&"tag".to_string()),
            issues_events: config.events.contains(&"issues".to_string()),
            note_events: false,
            pipeline_events: config.events.contains(&"pipeline".to_string()),
        };

        let response = self
            .send_with_retry(|| client.post(&url).headers(headers.clone()).json(&request))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to create webhook: {}",
                response.status()
            )));
        }

        let hook: HookResponse = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        Ok(hook.id.to_string())
    }

    async fn get_user(&self, access_token: &str) -> Result<User, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let url = format!("{}/api/v4/user", self.base_url);

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to get user: {}",
                response.status()
            )));
        }

        #[derive(Deserialize)]
        struct GitLabUser {
            id: i64,
            username: String,
            name: String,
            email: Option<String>,
            avatar_url: Option<String>,
        }

        let user: GitLabUser = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        Ok(User {
            id: user.id.to_string(),
            username: user.username,
            name: Some(user.name),
            email: user.email,
            avatar_url: user.avatar_url,
        })
    }

    async fn get_latest_commit(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<Commit, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let project_path = format!("{}/{}", owner, repo);
        let encoded_path = urlencoding::encode(&project_path);
        let encoded_branch = urlencoding::encode(branch);
        let url = format!(
            "{}/api/v4/projects/{}/repository/commits/{}",
            self.base_url, encoded_path, encoded_branch
        );

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to get latest commit: {}",
                response.status()
            )));
        }

        let commit_response: GitLabCommitResponse = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        Ok(Commit {
            sha: commit_response.id,
            message: commit_response.message,
            author: commit_response.author_name,
            author_email: commit_response.author_email,
            date: chrono::DateTime::parse_from_rfc3339(&commit_response.committed_date)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    }

    async fn delete_webhook(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        webhook_id: &str,
    ) -> Result<(), GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let project_path = format!("{}/{}", owner, repo);
        let encoded_path = urlencoding::encode(&project_path);
        let url = format!(
            "{}/api/v4/projects/{}/hooks/{}",
            self.base_url, encoded_path, webhook_id
        );

        let response = self
            .send_with_retry(|| client.delete(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to delete webhook: {}",
                response.status()
            )));
        }

        Ok(())
    }

    async fn verify_webhook_signature(
        &self,
        _payload: &[u8],
        signature: &str,
        secret: &str,
    ) -> Result<bool, GitProviderError> {
        // GitLab uses X-Gitlab-Token for webhook verification
        // This is a simple token comparison, not HMAC-based like GitHub
        Ok(signature == secret)
    }

    async fn check_repository_accessible(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<bool, GitProviderError> {
        let client = self.get_client();

        let project_path = format!("{}/{}", owner, repo);
        let encoded_path = urlencoding::encode(&project_path);
        let url = format!("{}/api/v4/projects/{}", self.base_url, encoded_path);

        let response = self.send_with_retry(|| client.get(&url)).await?;

        Ok(response.status().is_success())
    }

    async fn clone_repository(
        &self,
        clone_url: &str,
        target_dir: &str,
        access_token: Option<&str>,
    ) -> Result<(), GitProviderError> {
        let clone_url = clone_url.to_string();
        let target_dir = std::path::PathBuf::from(target_dir);
        let access_token = access_token.map(|s| s.to_string());

        tokio::task::spawn_blocking(move || {
            let target = target_dir.as_path();
            if let Some(token) = &access_token {
                // GitLab uses "oauth2" as the username for token auth
                super::git_ops::clone_repo_with_credentials(
                    &clone_url, target, "oauth2", token, None,
                )
            } else {
                super::git_ops::clone_repo(&clone_url, target, None)
            }
        })
        .await
        .map_err(|e| GitProviderError::Other(format!("Git clone task failed: {}", e)))?
        .map_err(|e| GitProviderError::Other(format!("Git clone failed: {}", e)))?;

        Ok(())
    }

    async fn get_commit(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        reference: &str,
    ) -> Result<Commit, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        // URL encode the project path (owner/repo)
        let project_path = format!("{}/{}", owner, repo);
        let encoded_project = urlencoding::encode(&project_path);
        let encoded_reference = urlencoding::encode(reference);

        // GitLab API endpoint for getting a commit
        let url = format!(
            "{}/api/v4/projects/{}/repository/commits/{}",
            self.base_url, encoded_project, encoded_reference
        );

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(GitProviderError::ApiError(format!(
                "Failed to get commit: {} - {}",
                status, error_text
            )));
        }

        #[derive(Deserialize)]
        struct GitLabCommit {
            id: String,
            message: String,
            author_name: String,
            author_email: String,
            created_at: String,
        }

        let gitlab_commit: GitLabCommit = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        let date = chrono::DateTime::parse_from_rfc3339(&gitlab_commit.created_at)
            .map_err(|e| GitProviderError::ApiError(format!("Failed to parse date: {}", e)))?
            .with_timezone(&chrono::Utc);

        Ok(Commit {
            sha: gitlab_commit.id,
            message: gitlab_commit.message,
            author: gitlab_commit.author_name,
            author_email: gitlab_commit.author_email,
            date,
        })
    }

    async fn check_commit_exists(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        commit_sha: &str,
    ) -> Result<bool, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        // URL encode the project path (owner/repo)
        let project_path = format!("{}/{}", owner, repo);
        let encoded_project = urlencoding::encode(&project_path);

        // GitLab API endpoint for getting a commit
        let url = format!(
            "{}/api/v4/projects/{}/repository/commits/{}",
            self.base_url, encoded_project, commit_sha
        );

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        // If we get a 200, the commit exists
        // If we get a 404, the commit doesn't exist
        // Other errors are actual errors
        match response.status() {
            status if status.is_success() => Ok(true),
            status if status == 404 => Ok(false),
            _ => {
                let error_text = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "Unknown error".to_string());
                Err(GitProviderError::ApiError(format!(
                    "Failed to check commit: {}",
                    error_text
                )))
            }
        }
    }

    async fn list_commits(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        branch: &str,
        per_page: u32,
    ) -> Result<Vec<Commit>, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let project_path = format!("{}/{}", owner, repo);
        let encoded_path = urlencoding::encode(&project_path);
        let url = format!(
            "{}/api/v4/projects/{}/repository/commits?ref_name={}&per_page={}",
            self.base_url, encoded_path, branch, per_page
        );

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to list commits: {}",
                response.status()
            )));
        }

        let items: Vec<GitLabCommitResponse> = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        let commits = items
            .into_iter()
            .map(|item| {
                let date = chrono::DateTime::parse_from_rfc3339(&item.committed_date)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|_| chrono::Utc::now());

                Commit {
                    sha: item.id,
                    message: item.message,
                    author: item.author_name,
                    author_email: item.author_email,
                    date,
                }
            })
            .collect();

        Ok(commits)
    }

    async fn download_archive(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        ref_spec: &str,
        target_path: &std::path::Path,
    ) -> Result<(), GitProviderError> {
        info!(
            "Downloading GitLab archive for {}/{} at ref {}",
            owner, repo, ref_spec
        );

        // URL encode the project path (owner/repo)
        let project_path = format!("{}/{}", owner, repo);
        let encoded_project = urlencoding::encode(&project_path);
        let encoded_ref = urlencoding::encode(ref_spec);

        // Build the URL for downloading the archive (GitLab uses tar.gz by default)
        let url = format!(
            "{}/api/v4/projects/{}/repository/archive.tar.gz?sha={}",
            self.base_url, encoded_project, encoded_ref
        );

        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(GitProviderError::ApiError(format!(
                "Failed to download archive: {} - {}",
                status, error_text
            )));
        }

        // Stream the response body to a file
        let mut file = tokio::fs::File::create(target_path)
            .await
            .map_err(|e| GitProviderError::Other(format!("Failed to create file: {}", e)))?;

        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|e| GitProviderError::ApiError(format!("Failed to read chunk: {}", e)))?;
            use tokio::io::AsyncWriteExt;
            file.write_all(&chunk)
                .await
                .map_err(|e| GitProviderError::Other(format!("Failed to write chunk: {}", e)))?;
        }

        info!(
            "Successfully downloaded GitLab archive to {:?}",
            target_path
        );
        Ok(())
    }

    async fn create_source(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        reference: &str,
    ) -> Result<Box<dyn temps_presets::source::ProjectSource>, GitProviderError> {
        // GitLab uses "namespace/project" format
        let project_id = format!("{}/{}", owner, repo);

        Ok(Box::new(crate::sources::GitLabSource::new(
            std::sync::Arc::new(self.get_client()),
            self.base_url.clone(),
            project_id,
            reference.to_string(),
            access_token.to_string(),
        )))
    }

    async fn create_repository(
        &self,
        access_token: &str,
        name: &str,
        owner: Option<&str>,
        description: Option<&str>,
        private: bool,
    ) -> Result<Repository, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        // GitLab API endpoint for creating projects
        let url = format!("{}/api/v4/projects", self.base_url);

        #[derive(Serialize)]
        struct CreateProjectRequest {
            name: String,
            path: String,
            description: Option<String>,
            visibility: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            namespace_id: Option<String>,
            initialize_with_readme: bool,
        }

        // Get namespace ID if owner is specified
        let namespace_id = if let Some(namespace) = owner {
            // Try to find the namespace/group ID
            let namespace_url = format!("{}/api/v4/namespaces?search={}", self.base_url, namespace);
            let headers_clone = headers.clone();
            let namespace_response = self
                .send_with_retry(|| client.get(&namespace_url).headers(headers_clone.clone()))
                .await
                .map_err(|e| {
                    GitProviderError::ApiError(format!("Failed to find namespace: {}", e))
                })?;

            if namespace_response.status().is_success() {
                #[derive(Deserialize)]
                struct Namespace {
                    id: i64,
                    path: String,
                }
                let namespaces: Vec<Namespace> = namespace_response.json().await.map_err(|e| {
                    GitProviderError::ApiError(format!("Failed to parse namespaces: {}", e))
                })?;

                namespaces
                    .into_iter()
                    .find(|n| n.path == namespace)
                    .map(|n| n.id.to_string())
            } else {
                None
            }
        } else {
            None
        };

        let visibility = if private { "private" } else { "public" };

        let request = CreateProjectRequest {
            name: name.to_string(),
            path: name.to_string(),
            description: description.map(|s| s.to_string()),
            visibility: visibility.to_string(),
            namespace_id,
            initialize_with_readme: true, // Initialize with README to have a default branch
        };

        info!(
            "Creating GitLab repository {} (visibility: {})",
            name, visibility
        );

        let response = self
            .send_with_retry(|| client.post(&url).headers(headers.clone()).json(&request))
            .await
            .map_err(|e| {
                GitProviderError::ApiError(format!("Failed to create repository: {}", e))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!(
                "Failed to create GitLab repository: {} - {}",
                status, error_text
            );
            return Err(GitProviderError::ApiError(format!(
                "Failed to create repository: {} - {}",
                status, error_text
            )));
        }

        #[derive(Deserialize)]
        struct GitLabProject {
            id: i64,
            path: String,
            path_with_namespace: String,
            description: Option<String>,
            visibility: String,
            default_branch: Option<String>,
            http_url_to_repo: String,
            ssh_url_to_repo: String,
            web_url: String,
            star_count: i32,
            forks_count: i32,
            created_at: String,
            last_activity_at: String,
        }

        let project: GitLabProject = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to parse response: {}", e)))?;

        // Owner is everything in path_with_namespace except the trailing slug.
        let owner = project
            .path_with_namespace
            .rsplit_once('/')
            .map(|(ns, _)| ns.to_string())
            .unwrap_or_default();

        info!(
            "Successfully created GitLab repository: {}",
            project.path_with_namespace
        );

        Ok(Repository {
            id: project.id.to_string(),
            name: project.path,
            full_name: project.path_with_namespace,
            owner,
            description: project.description,
            private: project.visibility != "public",
            default_branch: project.default_branch.unwrap_or_else(|| "main".to_string()),
            clone_url: project.http_url_to_repo,
            ssh_url: project.ssh_url_to_repo,
            web_url: project.web_url,
            language: None,
            size: 0,
            stars: project.star_count,
            forks: project.forks_count,
            created_at: chrono::DateTime::parse_from_rfc3339(&project.created_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
            updated_at: chrono::DateTime::parse_from_rfc3339(&project.last_activity_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
            pushed_at: None,
        })
    }

    async fn push_files_to_repository(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        branch: &str,
        files: Vec<(String, Vec<u8>)>,
        commit_message: &str,
    ) -> Result<Commit, GitProviderError> {
        use base64::{engine::general_purpose::STANDARD, Engine};

        let client = self.get_client();
        let headers = self.get_headers(access_token);

        info!(
            "Pushing {} files to {}/{} on branch {}",
            files.len(),
            owner,
            repo,
            branch
        );

        // URL encode the project path
        let project_path = format!("{}/{}", owner, repo);
        let encoded_path = urlencoding::encode(&project_path);

        // GitLab API endpoint for commits (allows multi-file commits)
        let url = format!(
            "{}/api/v4/projects/{}/repository/commits",
            self.base_url, encoded_path
        );

        // Build actions for each file
        let actions: Vec<serde_json::Value> = files
            .into_iter()
            .map(|(path, content)| {
                // Try to decode as UTF-8 first; if not possible, use base64
                match String::from_utf8(content.clone()) {
                    Ok(text_content) => {
                        serde_json::json!({
                            "action": "create",
                            "file_path": path,
                            "content": text_content
                        })
                    }
                    Err(_) => {
                        serde_json::json!({
                            "action": "create",
                            "file_path": path,
                            "content": STANDARD.encode(&content),
                            "encoding": "base64"
                        })
                    }
                }
            })
            .collect();

        let commit_request = serde_json::json!({
            "branch": branch,
            "commit_message": commit_message,
            "actions": actions
        });

        let response = self
            .send_with_retry(|| {
                client
                    .post(&url)
                    .headers(headers.clone())
                    .json(&commit_request)
            })
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to create commit: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(GitProviderError::ApiError(format!(
                "Failed to push files: {} - {}",
                status, error_text
            )));
        }

        #[derive(Deserialize)]
        struct CommitResponse {
            id: String,
            message: String,
            author_name: String,
            author_email: String,
            created_at: String,
        }

        let commit_response: CommitResponse = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse commit response: {}", e))
        })?;

        info!(
            "Successfully pushed files to {}/{} with commit {}",
            owner, repo, commit_response.id
        );

        Ok(Commit {
            sha: commit_response.id,
            message: commit_response.message,
            author: commit_response.author_name,
            author_email: commit_response.author_email,
            date: chrono::DateTime::parse_from_rfc3339(&commit_response.created_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
    }

    async fn create_pull_request(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        title: &str,
        body: &str,
        head_branch: &str,
        base_branch: &str,
    ) -> Result<PullRequest, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        // URL encode the project path (owner/repo)
        let project_path = format!("{}/{}", owner, repo);
        let encoded_path = urlencoding::encode(&project_path);

        let url = format!(
            "{}/api/v4/projects/{}/merge_requests",
            self.base_url, encoded_path
        );

        #[derive(Serialize)]
        struct CreateMergeRequestBody<'a> {
            title: &'a str,
            description: &'a str,
            source_branch: &'a str,
            target_branch: &'a str,
        }

        let request_body = CreateMergeRequestBody {
            title,
            description: body,
            source_branch: head_branch,
            target_branch: base_branch,
        };

        info!(
            "Creating merge request '{}' in {}/{}: {} -> {}",
            title, owner, repo, head_branch, base_branch
        );

        let response = self
            .send_with_retry(|| {
                client
                    .post(&url)
                    .headers(headers.clone())
                    .json(&request_body)
            })
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!(
                "Failed to create merge request in {}/{}: {} - {}",
                owner, repo, status, error_text
            );
            return Err(GitProviderError::ApiError(format!(
                "Failed to create merge request in {}/{}: {} - {}",
                owner, repo, status, error_text
            )));
        }

        #[derive(Deserialize)]
        struct GitLabMergeRequest {
            iid: i32,
            web_url: String,
            title: String,
            source_branch: String,
            target_branch: String,
            sha: Option<String>,
        }

        let mr: GitLabMergeRequest = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse merge request response: {}", e))
        })?;

        info!(
            "Successfully created merge request !{} in {}/{}",
            mr.iid, owner, repo
        );

        Ok(PullRequest {
            number: mr.iid,
            url: mr.web_url,
            title: mr.title,
            head_branch: mr.source_branch,
            base_branch: mr.target_branch,
            head_sha: mr.sha,
        })
    }
}
