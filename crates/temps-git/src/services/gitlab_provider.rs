use super::git_provider::{
    AuthMethod, Branch, Commit, FileContent, GitProviderError, GitProviderService, GitProviderTag,
    GitProviderType, PullRequest, RepoDirEntry, Repository, RepositoryPage, User, WebhookConfig,
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
            // SSRF defense-in-depth: never follow redirects — a public host
            // could otherwise 302 to an internal address (e.g. cloud metadata)
            // after URL validation has already passed at create time.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("Failed to build reqwest client with static config")
    }

    /// Client for streaming archive downloads. Uses a *generous* 15-minute total
    /// timeout (the 30s API timeout would abort a large archive mid-stream) plus
    /// tighter connect + per-read-inactivity bounds. The total timeout is the
    /// hard backstop guaranteeing the request can never hang forever, covering
    /// every phase (connect, response headers, body) — ample for large repos.
    fn get_archive_client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .user_agent("Temps-Engine/1.0")
            .timeout(std::time::Duration::from_secs(15 * 60))
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("Failed to build reqwest archive client with static config")
    }

    /// SSRF guard for archive-download redirects.
    ///
    /// GitLab's archive endpoint streams the body directly today, so the happy
    /// path never redirects. But self-hosted GitLab behind a CDN/object store
    /// *can* 302 to a signed URL, and the archive client uses
    /// `redirect::Policy::none()` — so if we ever follow a redirect manually it
    /// MUST be gated here, exactly like the GitHub provider. Without this guard a
    /// compromised or misconfigured upstream could bounce the download to an
    /// internal address (e.g. cloud metadata at 169.254.169.254).
    ///
    /// Only HTTPS redirects to the GitLab instance's own registrable domain are
    /// allowed: for `base_url = https://gitlab.example.com` that's
    /// `gitlab.example.com` and any `*.gitlab.example.com` (covers an object-store
    /// subdomain), plus the public `*.gitlab.com` / `*.gitlab-static.net` hosts.
    fn validate_archive_redirect_host(
        &self,
        redirect_url: &reqwest::Url,
    ) -> Result<(), GitProviderError> {
        if redirect_url.scheme() != "https" {
            return Err(GitProviderError::ApiError(format!(
                "Refusing to follow archive redirect to non-HTTPS URL: {}",
                redirect_url
            )));
        }

        let host = redirect_url
            .host_str()
            .ok_or_else(|| {
                GitProviderError::ApiError(format!(
                    "Archive redirect URL has no host: {}",
                    redirect_url
                ))
            })?
            .to_ascii_lowercase();

        // Public gitlab.com archive/object-store hosts.
        const ALLOWED_SUFFIXES: [&str; 2] = [
            ".gitlab.com",        // *.gitlab.com
            ".gitlab-static.net", // gitlab.com's object storage CDN
        ];
        let allowed_public =
            host == "gitlab.com" || ALLOWED_SUFFIXES.iter().any(|suffix| host.ends_with(suffix));

        // Self-hosted: allow the configured instance host and its subdomains.
        // e.g. base_url `https://gitlab.example.com` permits `gitlab.example.com`
        // and `*.gitlab.example.com` (an object-store subdomain).
        let allowed_instance = reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
            .map(|base_host| host == base_host || host.ends_with(&format!(".{}", base_host)))
            .unwrap_or(false);

        if allowed_public || allowed_instance {
            Ok(())
        } else {
            Err(GitProviderError::ApiError(format!(
                "Refusing to follow archive redirect to non-GitLab host '{}' (from {})",
                host, redirect_url
            )))
        }
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

        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let project_path = format!("{}/{}", owner, repo);
        let encoded_path = urlencoding::encode(&project_path);

        // GitLab defaults to 20 results per page; without an explicit pager the
        // selected branch may fall outside the first page on busy repos and the
        // UI then can't show it. Mirror the public-repo pattern: per_page=100,
        // walk pages until short or we hit the 1000-branch safety cap.
        let mut all_branches: Vec<Branch> = Vec::new();
        let mut page: u32 = 1;
        let per_page: usize = 100;

        loop {
            let url = format!(
                "{}/api/v4/projects/{}/repository/branches?per_page={}&page={}",
                self.base_url, encoded_path, per_page, page
            );

            let response = self
                .send_with_retry(|| client.get(&url).headers(headers.clone()))
                .await?;

            if !response.status().is_success() {
                return Err(GitProviderError::ApiError(format!(
                    "Failed to list branches for {}/{} (page {}): {}",
                    owner,
                    repo,
                    page,
                    response.status()
                )));
            }

            let gitlab_branches: Vec<GitLabBranch> = response
                .json()
                .await
                .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

            let count = gitlab_branches.len();
            all_branches.extend(gitlab_branches.into_iter().map(|b| Branch {
                name: b.name,
                commit_sha: b.commit.id,
                protected: b.protected,
            }));

            if count < per_page || all_branches.len() >= 1000 {
                break;
            }
            page += 1;
        }

        Ok(all_branches)
    }

    async fn list_tags(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<GitProviderTag>, GitProviderError> {
        #[derive(Deserialize)]
        struct GitLabTag {
            name: String,
            commit: GitLabCommitRef,
        }

        #[derive(Deserialize)]
        struct GitLabCommitRef {
            id: String,
        }

        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let project_path = format!("{}/{}", owner, repo);
        let encoded_path = urlencoding::encode(&project_path);

        // Paginate: GitLab's default 20-per-page truncates large tag sets. Cap
        // at 1000 tags to bound memory on huge repos.
        let mut all_tags: Vec<GitProviderTag> = Vec::new();
        let mut page: u32 = 1;
        let per_page: usize = 100;

        loop {
            let url = format!(
                "{}/api/v4/projects/{}/repository/tags?per_page={}&page={}",
                self.base_url, encoded_path, per_page, page
            );

            let response = self
                .send_with_retry(|| client.get(&url).headers(headers.clone()))
                .await?;

            if !response.status().is_success() {
                return Err(GitProviderError::ApiError(format!(
                    "Failed to list tags for {}/{} (page {}): {}",
                    owner,
                    repo,
                    page,
                    response.status()
                )));
            }

            let gitlab_tags: Vec<GitLabTag> = response.json().await.map_err(|e| {
                GitProviderError::ApiError(format!("Failed to parse tags response: {}", e))
            })?;

            let count = gitlab_tags.len();
            all_tags.extend(gitlab_tags.into_iter().map(|t| GitProviderTag {
                name: t.name,
                commit_sha: t.commit.id,
            }));

            if count < per_page || all_tags.len() >= 1000 {
                break;
            }
            page += 1;
        }

        Ok(all_tags)
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

    async fn list_directory(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        path: &str,
        reference: Option<&str>,
    ) -> Result<Vec<RepoDirEntry>, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        // GitLab project identifier is the URL-encoded `owner/repo` path.
        let project_path = format!("{}/{}", owner, repo);
        let encoded_project = urlencoding::encode(&project_path);

        // Collect up to 300 entries across at most three pages of 100 each.
        // The GitLab repository tree API is non-recursive by default; each
        // response page covers one level of the tree.
        const PER_PAGE: usize = 100;
        const MAX_PAGES: u32 = 3;

        let mut all_entries: Vec<RepoDirEntry> = Vec::new();
        let mut page: u32 = 1;

        loop {
            // Build the query string manually so we only add params that are
            // meaningful. `path` is omitted (empty string maps to root) and
            // `ref` is omitted when the caller didn't specify one.
            let mut query_parts: Vec<String> = vec![format!("per_page={}", PER_PAGE)];
            query_parts.push(format!("page={}", page));

            // GitLab treats an empty path as "root" — only add the param when
            // there's an actual path to request.
            if !path.is_empty() {
                query_parts.push(format!("path={}", urlencoding::encode(path)));
            }

            if let Some(ref_name) = reference {
                if !ref_name.is_empty() {
                    query_parts.push(format!("ref={}", urlencoding::encode(ref_name)));
                }
            }

            let url = format!(
                "{}/api/v4/projects/{}/repository/tree?{}",
                self.base_url,
                encoded_project,
                query_parts.join("&")
            );

            let response = self
                .send_with_retry(|| client.get(&url).headers(headers.clone()))
                .await?;

            if !response.status().is_success() {
                return Err(GitProviderError::ApiError(format!(
                    "Failed to list directory '{path}' in {owner}/{repo} (page {page}): {}",
                    response.status()
                )));
            }

            #[derive(Deserialize)]
            struct GitLabTreeItem {
                name: String,
                path: String,
                #[serde(rename = "type")]
                item_type: String,
            }

            let items: Vec<GitLabTreeItem> = response.json().await.map_err(|e| {
                GitProviderError::ApiError(format!(
                    "Failed to parse directory listing for '{path}' in {owner}/{repo}: {e}"
                ))
            })?;

            let count = items.len();
            all_entries.extend(items.into_iter().map(|item| {
                let is_dir = item.item_type == "tree";
                RepoDirEntry {
                    name: item.name,
                    path: item.path,
                    is_dir,
                    // GitLab repository tree API does not return file sizes.
                    size: None,
                }
            }));

            // Stop if the page was short (last page) or we've hit the cap.
            if count < PER_PAGE || page >= MAX_PAGES {
                break;
            }
            page += 1;
        }

        // Sort: directories first, then alphabetically by name within each group.
        all_entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

        Ok(all_entries)
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

        // libgit2's clone has no network timeout — a stalled fetch would hang
        // the deployment indefinitely. Bound it so the job fails fast instead.
        const CLONE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

        let join = tokio::task::spawn_blocking(move || {
            let target = target_dir.as_path();
            if let Some(token) = &access_token {
                // GitLab uses "oauth2" as the username for token auth
                super::git_ops::clone_repo_with_credentials(
                    &clone_url, target, "oauth2", token, None,
                )
            } else {
                super::git_ops::clone_repo(&clone_url, target, None)
            }
        });

        match tokio::time::timeout(CLONE_TIMEOUT, join).await {
            Ok(joined) => {
                joined
                    .map_err(|e| GitProviderError::Other(format!("Git clone task failed: {}", e)))?
                    .map_err(|e| GitProviderError::Other(format!("Git clone failed: {}", e)))?;
                Ok(())
            }
            Err(_) => Err(GitProviderError::Other(format!(
                "Git clone timed out after {}s",
                CLONE_TIMEOUT.as_secs()
            ))),
        }
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
            if status == reqwest::StatusCode::NOT_FOUND {
                return Err(GitProviderError::CommitNotFound {
                    repository: format!("{}/{}", owner, repo),
                    commit_sha: reference.to_string(),
                });
            }
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
        progress: Option<&crate::services::git_provider::ArchiveProgressSender>,
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

        // Archive client: no total timeout so large archives can stream fully.
        let client = self.get_archive_client();
        let headers = self.get_headers(access_token);

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        // GitLab usually streams the archive directly, but self-hosted instances
        // behind a CDN/object store can answer with a 302 to a signed URL. The
        // archive client uses `redirect::Policy::none()` (SSRF defense), so follow
        // that one hop manually — but only after validating the target host, and
        // without forwarding the auth token (the signed URL is self-authenticating
        // and forwarding the token to a redirect target would needlessly leak it).
        let response = if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    GitProviderError::ApiError(format!(
                        "Archive download for {}/{} returned {} with no Location header",
                        owner,
                        repo,
                        response.status()
                    ))
                })?
                .to_string();

            let redirect_url = reqwest::Url::parse(&location).map_err(|e| {
                GitProviderError::ApiError(format!(
                    "Archive redirect Location is not a valid URL ({}): {}",
                    location, e
                ))
            })?;

            self.validate_archive_redirect_host(&redirect_url)?;
            debug!(
                "Archive: following validated redirect to host {}",
                redirect_url.host_str().unwrap_or("?")
            );

            client.get(redirect_url).send().await.map_err(|e| {
                GitProviderError::ApiError(format!(
                    "Failed to download archive from redirect target: {}",
                    e
                ))
            })?
        } else {
            response
        };

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

        // Cap total bytes written so an unexpectedly huge (or hostile) archive
        // can't fill the control-plane volume; the client timeout bounds stall
        // time, not stream size.
        const MAX_ARCHIVE_BYTES: u64 = 5 * 1024 * 1024 * 1024; // 5 GiB
        if let Some(len) = response.content_length() {
            if len > MAX_ARCHIVE_BYTES {
                return Err(GitProviderError::ApiError(format!(
                    "Archive too large: Content-Length {} exceeds limit {} bytes",
                    len, MAX_ARCHIVE_BYTES
                )));
            }
        }

        let total_bytes = response.content_length();

        // Stream the response body to a file
        let mut file = tokio::fs::File::create(target_path)
            .await
            .map_err(|e| GitProviderError::Other(format!("Failed to create file: {}", e)))?;

        const PROGRESS_STEP: u64 = 512 * 1024;
        let mut next_progress_at: u64 = PROGRESS_STEP;
        let mut written: u64 = 0;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|e| GitProviderError::ApiError(format!("Failed to read chunk: {}", e)))?;
            written = written.saturating_add(chunk.len() as u64);
            // Enforce the size ceiling BEFORE writing this chunk so an oversized
            // stream can't put even one over-limit chunk on disk.
            if written > MAX_ARCHIVE_BYTES {
                drop(file);
                let _ = tokio::fs::remove_file(target_path).await;
                return Err(GitProviderError::ApiError(format!(
                    "Archive exceeded maximum size of {} bytes mid-stream",
                    MAX_ARCHIVE_BYTES
                )));
            }
            if let Some(tx) = progress {
                if written >= next_progress_at {
                    let _ = tx.send(crate::services::git_provider::ArchiveProgress {
                        downloaded_bytes: written,
                        total_bytes,
                    });
                    next_progress_at = written.saturating_add(PROGRESS_STEP);
                }
            }
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

#[cfg(test)]
mod archive_redirect_tests {
    use super::*;

    fn provider(base_url: Option<&str>) -> GitLabProvider {
        GitLabProvider::new(
            base_url.map(String::from),
            AuthMethod::PersonalAccessToken {
                token: "t".to_string(),
            },
        )
    }

    fn check(base_url: Option<&str>, url: &str) -> Result<(), GitProviderError> {
        provider(base_url).validate_archive_redirect_host(&reqwest::Url::parse(url).unwrap())
    }

    #[test]
    fn allows_public_gitlab_hosts() {
        // Default base_url is https://gitlab.com.
        assert!(check(None, "https://gitlab.com/owner/repo/-/archive/main.tar.gz").is_ok());
        assert!(check(None, "https://storage.gitlab-static.net/foo").is_ok());
        assert!(check(None, "https://cdn.gitlab.com/foo").is_ok());
    }

    #[test]
    fn allows_self_hosted_instance_and_subdomains() {
        // Self-hosted base host + an object-store subdomain under it.
        assert!(check(
            Some("https://gitlab.example.com"),
            "https://gitlab.example.com/foo"
        )
        .is_ok());
        assert!(check(
            Some("https://gitlab.example.com"),
            "https://objects.gitlab.example.com/signed/blob"
        )
        .is_ok());
    }

    #[test]
    fn rejects_internal_and_metadata_targets() {
        // The SSRF cases this guard exists for.
        assert!(check(None, "https://169.254.169.254/latest/meta-data/").is_err());
        assert!(check(None, "https://localhost/foo").is_err());
        assert!(check(None, "https://10.0.0.5/foo").is_err());
    }

    #[test]
    fn rejects_non_https_redirect() {
        assert!(check(None, "http://gitlab.com/owner/repo").is_err());
    }

    #[test]
    fn rejects_lookalike_and_suffix_spoof_hosts() {
        // Attacker-controlled domains that merely *contain* gitlab text.
        assert!(check(None, "https://gitlab.com.evil.example/foo").is_err());
        assert!(check(None, "https://evilgitlab.com/foo").is_err());
        assert!(check(None, "https://notgitlab-static.net/foo").is_err());
        // A self-hosted instance host must not leak access to an unrelated host.
        assert!(check(
            Some("https://gitlab.example.com"),
            "https://evil.example/foo"
        )
        .is_err());
        // ...and a self-hosted instance does NOT implicitly trust public gitlab.com
        // is the inverse — public is always allowed, which is intended (mirrors the
        // tarball flow); the spoof guard is the suffix-boundary check above.
    }

    #[test]
    fn userinfo_uses_connect_host_not_userinfo() {
        // `host_str()` returns the real connect host, so userinfo can't spoof it.
        assert!(check(None, "https://evil.example@gitlab.com/foo").is_ok());
        assert!(check(None, "https://gitlab.com@evil.example/foo").is_err());
    }

    #[test]
    fn rejects_trailing_dot_fqdn() {
        // `gitlab.com.` ends with `com.`, not `.gitlab.com` → rejected.
        assert!(check(None, "https://gitlab.com./foo").is_err());
    }
}

#[cfg(test)]
mod list_directory_tests {
    use super::*;

    /// Simulate the mapping + sort logic applied in `list_directory` without
    /// making any HTTP calls.
    fn map_and_sort(items: Vec<(&str, &str, &str)>) -> Vec<RepoDirEntry> {
        // (name, path, type)
        let mut entries: Vec<RepoDirEntry> = items
            .into_iter()
            .map(|(name, path, item_type)| {
                let is_dir = item_type == "tree";
                RepoDirEntry {
                    name: name.to_string(),
                    path: path.to_string(),
                    is_dir,
                    // GitLab tree API does not return size.
                    size: None,
                }
            })
            .collect();

        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        entries
    }

    #[test]
    fn dirs_sorted_before_files() {
        let entries = map_and_sort(vec![
            ("main.rs", "src/main.rs", "blob"),
            ("lib", "lib", "tree"),
            ("README.md", "README.md", "blob"),
            ("src", "src", "tree"),
        ]);

        assert!(entries[0].is_dir);
        assert!(entries[1].is_dir);
        assert_eq!(entries[0].name, "lib");
        assert_eq!(entries[1].name, "src");
        assert!(!entries[2].is_dir);
        assert!(!entries[3].is_dir);
        assert_eq!(entries[2].name, "README.md");
        assert_eq!(entries[3].name, "main.rs");
    }

    #[test]
    fn size_is_always_none_for_gitlab() {
        let entries = map_and_sort(vec![
            ("Cargo.toml", "Cargo.toml", "blob"),
            ("src", "src", "tree"),
        ]);

        for entry in &entries {
            assert_eq!(
                entry.size, None,
                "GitLab tree API does not surface file sizes"
            );
        }
    }

    #[test]
    fn empty_directory_returns_empty_vec() {
        let entries = map_and_sort(vec![]);
        assert!(entries.is_empty());
    }

    #[test]
    fn blob_entry_is_not_a_dir() {
        let entries = map_and_sort(vec![("Makefile", "Makefile", "blob")]);
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].is_dir);
    }

    #[test]
    fn tree_entry_is_a_dir() {
        let entries = map_and_sort(vec![("tests", "tests", "tree")]);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_dir);
    }
}
