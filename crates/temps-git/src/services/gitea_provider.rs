use super::git_provider::{
    AuthMethod, Branch, Commit, FileContent, GitProviderError, GitProviderService, GitProviderTag,
    GitProviderType, PullRequest, RepoDirEntry, Repository, RepositoryPage, User, WebhookConfig,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Gitea (and Forgejo) REST API provider.
///
/// Implements `GitProviderService` for any self-hosted Gitea/Forgejo instance
/// via its v1 REST API at `{base_url}/api/v1`.
///
/// Authentication is PAT-only for v1. Gitea uses
/// `Authorization: token {pat}` for PAT authentication.
///
/// Security:
/// - Both reqwest clients use `redirect::Policy::none()` as SSRF defense-in-depth.
/// - Archive redirects are validated to the configured base domain only.
/// - `mint_scoped_repo_token` returns `NotImplemented`; callers fall back to
///   the stored long-lived PAT (same posture as GitLab PAT connections).
pub struct GiteaProvider {
    base_url: String,
    auth_method: AuthMethod,
}

impl GiteaProvider {
    /// Create a new Gitea provider.
    ///
    /// # Arguments
    /// * `base_url` — the Gitea instance web root, e.g. `https://git.example.com`.
    ///   Must be HTTPS-only (validated by the caller via `validate_git_url`).
    /// * `auth_method` — PAT is the only supported v1 auth method.
    pub fn new(base_url: String, auth_method: AuthMethod) -> Self {
        Self {
            base_url,
            auth_method,
        }
    }

    /// API base URL derived from the instance web root.
    fn api_base(&self) -> String {
        format!("{}/api/v1", self.base_url.trim_end_matches('/'))
    }

    /// Standard API client: 30-second timeout, no redirects (SSRF defense).
    fn get_client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .user_agent("Temps-Engine/1.0")
            .timeout(std::time::Duration::from_secs(30))
            // SSRF defense-in-depth: never follow redirects automatically.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("Failed to build reqwest client with static config")
    }

    /// Archive download client: 15-minute total timeout, no redirects.
    ///
    /// The generous total timeout allows large archives to stream fully while
    /// the connect/read timeouts bound stalled connections.
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
    /// Only HTTPS redirects whose host matches the registrable domain of
    /// `base_url` (or a subdomain of it) are allowed, preventing a
    /// compromised upstream from bouncing an archive download to an internal
    /// address or cloud metadata endpoint.
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

        // Allow the configured Gitea instance host and its subdomains (e.g.
        // an object-store CDN subdomain). Reject everything else.
        let allowed = reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
            .map(|base_host| host == base_host || host.ends_with(&format!(".{}", base_host)))
            .unwrap_or(false);

        if allowed {
            Ok(())
        } else {
            Err(GitProviderError::ApiError(format!(
                "Refusing to follow archive redirect to host '{}' not matching Gitea instance (from {})",
                host, redirect_url
            )))
        }
    }

    /// Retry configuration for Gitea API calls.
    fn retry_config() -> temps_core::retry::RetryConfig {
        temps_core::retry::RetryConfig::new(3)
            .with_base_delay(std::time::Duration::from_secs(1))
            .with_max_delay(std::time::Duration::from_secs(10))
    }

    /// Send an HTTP request with retry for transient failures (5xx / 429).
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

    /// Build the Authorization header for Gitea PAT authentication.
    ///
    /// Gitea uses `Authorization: token {pat}` — distinct from GitHub's Bearer
    /// and GitLab's PRIVATE-TOKEN.
    fn auth_header_value(&self, access_token: &str) -> String {
        match &self.auth_method {
            AuthMethod::PersonalAccessToken { .. } => format!("token {}", access_token),
            _ => format!("token {}", access_token),
        }
    }

    fn get_headers(&self, access_token: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&self.auth_header_value(access_token))
                .expect("PAT token must be a valid header value"),
        );
        headers
    }

    /// Validate the access token by calling `GET /api/v1/user`.
    async fn validate_token_internal(&self, access_token: &str) -> Result<bool, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);
        let url = format!("{}/user", self.api_base());

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await;

        match response {
            Ok(resp) => match resp.status() {
                s if s.is_success() => Ok(true),
                s if s.as_u16() == 401 || s.as_u16() == 403 => Ok(false),
                s => {
                    let text = resp.text().await.unwrap_or_default();
                    Err(GitProviderError::ApiError(format!(
                        "Unexpected response validating Gitea token: {} - {}",
                        s, text
                    )))
                }
            },
            Err(_) => Err(GitProviderError::RateLimitExceeded),
        }
    }
}

// ── Response types ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct GiteaUser {
    id: i64,
    login: String,
    full_name: Option<String>,
    email: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Deserialize)]
struct GiteaRepo {
    id: i64,
    name: String,
    full_name: String,
    #[serde(default)]
    description: Option<String>,
    private: bool,
    #[serde(default)]
    default_branch: Option<String>,
    clone_url: String,
    ssh_url: String,
    html_url: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    size: i64,
    #[serde(default)]
    stars_count: i32,
    #[serde(default)]
    forks_count: i32,
    /// Gitea ≥ 1.17 uses `created_at`; older versions used `created`.
    /// Accept both via serde alias.
    #[serde(alias = "created")]
    created_at: String,
    /// Gitea ≥ 1.17 uses `updated_at`; older versions used `updated`.
    /// Accept both via serde alias.
    #[serde(alias = "updated")]
    updated_at: String,
}

impl GiteaRepo {
    fn owner_from_full_name(&self) -> String {
        self.full_name
            .rsplit_once('/')
            .map(|(ns, _)| ns.to_string())
            .unwrap_or_default()
    }

    fn into_repository(self) -> Repository {
        let owner = self.owner_from_full_name();
        Repository {
            id: self.id.to_string(),
            name: self.name,
            full_name: self.full_name,
            owner,
            description: self.description,
            private: self.private,
            default_branch: self.default_branch.unwrap_or_else(|| "main".to_string()),
            clone_url: self.clone_url,
            ssh_url: self.ssh_url,
            web_url: self.html_url,
            language: self.language,
            size: self.size,
            stars: self.stars_count,
            forks: self.forks_count,
            created_at: chrono::DateTime::parse_from_rfc3339(&self.created_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
            updated_at: chrono::DateTime::parse_from_rfc3339(&self.updated_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
            pushed_at: None,
        }
    }
}

#[derive(Deserialize)]
struct GiteaBranch {
    name: String,
    commit: GiteaCommitRef,
}

#[derive(Deserialize)]
struct GiteaCommitRef {
    id: String,
}

#[derive(Deserialize)]
struct GiteaTag {
    name: String,
    id: String,
}

#[derive(Deserialize)]
struct GiteaFileContent {
    #[allow(dead_code)]
    name: String,
    path: String,
    content: Option<String>,
    encoding: Option<String>,
}

#[derive(Deserialize)]
struct GiteaCommit {
    sha: String,
    commit: GiteaCommitDetails,
}

#[derive(Deserialize)]
struct GiteaCommitDetails {
    message: String,
    author: GiteaCommitAuthor,
    committer: GiteaCommitCommitter,
}

#[derive(Deserialize)]
struct GiteaCommitAuthor {
    name: String,
    email: String,
}

#[derive(Deserialize)]
struct GiteaCommitCommitter {
    date: String,
}

#[derive(Deserialize)]
struct GiteaHookResponse {
    id: i64,
}

#[derive(Deserialize)]
struct GiteaSearchResult {
    data: Vec<GiteaRepo>,
}

#[derive(Deserialize)]
struct GiteaPullRequest {
    number: i64,
    url: String,
    title: String,
    head: GiteaBranchRef,
    base: GiteaBranchRef,
    head_commit_id: Option<String>,
}

#[derive(Deserialize)]
struct GiteaBranchRef {
    label: String,
}

// ── GitProviderService impl ──────────────────────────────────────────────────

#[async_trait]
impl GitProviderService for GiteaProvider {
    fn provider_type(&self) -> GitProviderType {
        GitProviderType::Gitea
    }

    async fn authenticate(&self, _code: Option<String>) -> Result<String, GitProviderError> {
        match &self.auth_method {
            AuthMethod::PersonalAccessToken { token } => Ok(token.clone()),
            _ => Err(GitProviderError::AuthenticationFailed(
                "Gitea only supports PAT authentication in v1. \
                 Create a token at Settings > Applications > Access Tokens."
                    .to_string(),
            )),
        }
    }

    async fn get_auth_url(&self, _state: &str) -> Result<String, GitProviderError> {
        Err(GitProviderError::InvalidConfiguration(
            "Gitea does not support OAuth in v1. \
             Use a Personal Access Token from Settings > Applications > Access Tokens."
                .to_string(),
        ))
    }

    async fn token_needs_refresh(&self, access_token: &str) -> bool {
        matches!(self.validate_token_internal(access_token).await, Ok(false))
    }

    async fn validate_token(&self, access_token: &str) -> Result<bool, GitProviderError> {
        self.validate_token_internal(access_token).await
    }

    async fn validate_and_refresh_token(
        &self,
        access_token: &str,
        _refresh_token: Option<&str>,
    ) -> Result<(String, Option<String>), GitProviderError> {
        // Gitea PATs do not expire and cannot be refreshed.
        match self.validate_token_internal(access_token).await {
            Ok(true) => Ok((access_token.to_string(), None)),
            Ok(false) => Err(GitProviderError::AuthenticationFailed(
                "Gitea Personal Access Token is invalid. \
                 Please create a new token in Settings > Applications > Access Tokens."
                    .to_string(),
            )),
            Err(e) => Err(e),
        }
    }

    async fn get_user(&self, access_token: &str) -> Result<User, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);
        let url = format!("{}/user", self.api_base());

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to get Gitea user: {}",
                response.status()
            )));
        }

        let user: GiteaUser = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to parse user: {}", e)))?;

        Ok(User {
            id: user.id.to_string(),
            username: user.login,
            name: user.full_name.filter(|s| !s.is_empty()),
            email: user.email,
            avatar_url: user.avatar_url,
        })
    }

    async fn list_repositories(
        &self,
        access_token: &str,
        organization: Option<&str>,
    ) -> Result<Vec<Repository>, GitProviderError> {
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

        const LIMIT: u32 = 50;
        let page = page.max(1);

        // Gitea 1.12+ has `/repos/search`; fall back to `/user/repos` on 404.
        // We always try search first because it handles both user and org repos.
        let (url, is_org_url) = if let Some(org) = organization {
            (
                format!(
                    "{}/repos/search?q=&limit={}&page={}&topic=false&includeDesc=true\
                     &owner={}&is_private=true",
                    self.api_base(),
                    LIMIT,
                    page,
                    urlencoding::encode(org)
                ),
                false,
            )
        } else {
            (
                format!(
                    "{}/repos/search?limit={}&page={}&token={}",
                    self.api_base(),
                    LIMIT,
                    page,
                    // `token` param is redundant with the header but some older
                    // Gitea versions need it for the search endpoint.
                    "" // header auth is canonical
                ),
                false,
            )
        };
        let _ = is_org_url;

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        // Gitea 1.12+ returns search results; older instances return 404.
        // On 404 fall back to /user/repos.
        let (repos, total_count_header): (Vec<GiteaRepo>, Option<u32>) = if response
            .status()
            .as_u16()
            == 404
        {
            debug!("Gitea /repos/search returned 404, falling back to /user/repos");
            let fallback_url = format!(
                "{}/user/repos?limit={}&page={}",
                self.api_base(),
                LIMIT,
                page
            );
            let fb_resp = self
                .send_with_retry(|| client.get(&fallback_url).headers(headers.clone()))
                .await?;

            let total = fb_resp
                .headers()
                .get("x-total-count")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u32>().ok());

            if !fb_resp.status().is_success() {
                return Err(GitProviderError::ApiError(format!(
                    "Failed to list Gitea repositories (page {}): {}",
                    page,
                    fb_resp.status()
                )));
            }

            let repos: Vec<GiteaRepo> = fb_resp.json().await.map_err(|e| {
                GitProviderError::ApiError(format!("Failed to parse repositories: {}", e))
            })?;
            (repos, total)
        } else {
            if !response.status().is_success() {
                return Err(GitProviderError::ApiError(format!(
                    "Failed to list Gitea repositories (page {}): {}",
                    page,
                    response.status()
                )));
            }

            // Gitea /repos/search returns `{"data": [...], "ok": true}`.
            let total = response
                .headers()
                .get("x-total-count")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u32>().ok());

            let search: GiteaSearchResult = response.json().await.map_err(|e| {
                GitProviderError::ApiError(format!("Failed to parse Gitea search response: {}", e))
            })?;
            (search.data, total)
        };

        let received = repos.len() as u32;
        let items: Vec<Repository> = repos.into_iter().map(|r| r.into_repository()).collect();

        // Determine if there's a next page from X-Total-Count or page fullness.
        let next_page = match total_count_header {
            Some(total) if (page * LIMIT) < total => Some(page + 1),
            None if received == LIMIT => Some(page + 1),
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
        let url = format!("{}/repos/{}/{}", self.api_base(), owner, repo);

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to get Gitea repository {}/{}: {}",
                owner,
                repo,
                response.status()
            )));
        }

        let gitea_repo: GiteaRepo = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse repository: {}", e))
        })?;

        Ok(gitea_repo.into_repository())
    }

    async fn list_branches(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<Branch>, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let mut all_branches: Vec<Branch> = Vec::new();
        let mut page: u32 = 1;
        let per_page: usize = 50;

        loop {
            let url = format!(
                "{}/repos/{}/{}/branches?limit={}&page={}",
                self.api_base(),
                owner,
                repo,
                per_page,
                page
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

            let branches: Vec<GiteaBranch> = response.json().await.map_err(|e| {
                GitProviderError::ApiError(format!("Failed to parse branches: {}", e))
            })?;

            let count = branches.len();
            all_branches.extend(branches.into_iter().map(|b| Branch {
                name: b.name,
                commit_sha: b.commit.id,
                protected: false, // Gitea branch API doesn't return protection status inline
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
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let mut all_tags: Vec<GitProviderTag> = Vec::new();
        let mut page: u32 = 1;
        let per_page: usize = 50;

        loop {
            let url = format!(
                "{}/repos/{}/{}/tags?limit={}&page={}",
                self.api_base(),
                owner,
                repo,
                per_page,
                page
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

            let tags: Vec<GiteaTag> = response
                .json()
                .await
                .map_err(|e| GitProviderError::ApiError(format!("Failed to parse tags: {}", e)))?;

            let count = tags.len();
            all_tags.extend(tags.into_iter().map(|t| GitProviderTag {
                name: t.name,
                commit_sha: t.id,
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

        let mut url = format!(
            "{}/repos/{}/{}/contents/{}",
            self.api_base(),
            owner,
            repo,
            urlencoding::encode(path)
        );
        if let Some(ref_name) = branch {
            url.push_str(&format!("?ref={}", urlencoding::encode(ref_name)));
        }

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to get file content for {}/{}/{}: {}",
                owner,
                repo,
                path,
                response.status()
            )));
        }

        let file: GiteaFileContent = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse file content: {}", e))
        })?;

        Ok(FileContent {
            path: file.path,
            content: file.content.unwrap_or_default(),
            encoding: file.encoding.unwrap_or_else(|| "base64".to_string()),
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

        // Percent-encode each path segment individually so `/` separators are
        // preserved but user-supplied characters can't break the URL.
        let encoded_path = path
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|segment| urlencoding::encode(segment).into_owned())
            .collect::<Vec<_>>()
            .join("/");

        // Gitea's Contents API mirrors GitHub's:
        // GET {api}/repos/{owner}/{repo}/contents/{path}?ref={ref}
        let mut url = format!(
            "{}/repos/{}/{}/contents/{}",
            self.api_base(),
            owner,
            repo,
            encoded_path
        );
        if let Some(ref_name) = reference {
            url.push_str(&format!("?ref={}", urlencoding::encode(ref_name)));
        }

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to list directory '{path}' in {owner}/{repo}: {}",
                response.status()
            )));
        }

        #[derive(Deserialize)]
        struct GiteaContentItem {
            name: String,
            path: String,
            #[serde(rename = "type")]
            item_type: String,
            size: Option<u64>,
        }

        let body_bytes = response.bytes().await.map_err(|e| {
            GitProviderError::ApiError(format!(
                "Failed to read directory listing response body: {e}"
            ))
        })?;

        // A directory returns a JSON array; a file returns a single object.
        let items: Vec<GiteaContentItem> =
            if let Ok(arr) = serde_json::from_slice::<Vec<GiteaContentItem>>(&body_bytes) {
                arr
            } else {
                match serde_json::from_slice::<GiteaContentItem>(&body_bytes) {
                    Ok(single) => vec![single],
                    Err(e) => {
                        return Err(GitProviderError::ApiError(format!(
                            "Failed to parse directory listing for '{path}' in {owner}/{repo}: {e}"
                        )));
                    }
                }
            };

        let mut entries: Vec<RepoDirEntry> = items
            .into_iter()
            .map(|item| {
                let is_dir = item.item_type == "dir";
                let size = if item.item_type == "file" {
                    item.size
                } else {
                    None
                };
                RepoDirEntry {
                    name: item.name,
                    path: item.path,
                    is_dir,
                    size,
                }
            })
            .collect();

        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

        Ok(entries)
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

        let url = format!(
            "{}/repos/{}/{}/commits?sha={}&limit=1",
            self.api_base(),
            owner,
            repo,
            urlencoding::encode(branch)
        );

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to get latest commit for {}/{} on {}: {}",
                owner,
                repo,
                branch,
                response.status()
            )));
        }

        let commits: Vec<GiteaCommit> = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to parse commits: {}", e)))?;

        commits
            .into_iter()
            .next()
            .map(|c| {
                let date = chrono::DateTime::parse_from_rfc3339(&c.commit.committer.date)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|_| chrono::Utc::now());
                Commit {
                    sha: c.sha,
                    message: c.commit.message,
                    author: c.commit.author.name,
                    author_email: c.commit.author.email,
                    date,
                }
            })
            .ok_or_else(|| {
                GitProviderError::ApiError(format!(
                    "No commits found for {}/{} on branch {}",
                    owner, repo, branch
                ))
            })
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

        let url = format!(
            "{}/repos/{}/{}/git/commits/{}",
            self.api_base(),
            owner,
            repo,
            urlencoding::encode(reference)
        );

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to get commit {} for {}/{}: {}",
                reference,
                owner,
                repo,
                response.status()
            )));
        }

        #[derive(Deserialize)]
        struct GiteaGitCommit {
            sha: String,
            message: String,
            author: GiteaGitCommitActor,
            committer: GiteaGitCommitActor,
        }

        #[derive(Deserialize)]
        struct GiteaGitCommitActor {
            name: String,
            email: String,
            date: String,
        }

        let commit: GiteaGitCommit = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse commit response: {}", e))
        })?;

        let date = chrono::DateTime::parse_from_rfc3339(&commit.committer.date)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());

        Ok(Commit {
            sha: commit.sha,
            message: commit.message,
            author: commit.author.name,
            author_email: commit.author.email,
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
        let url = format!(
            "{}/repos/{}/{}/git/commits/{}",
            self.api_base(),
            owner,
            repo,
            commit_sha
        );

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        match response.status() {
            s if s.is_success() => Ok(true),
            s if s.as_u16() == 404 => Ok(false),
            s => {
                let text = response.text().await.unwrap_or_default();
                Err(GitProviderError::ApiError(format!(
                    "Failed to check commit {}: {} - {}",
                    commit_sha, s, text
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
        let url = format!(
            "{}/repos/{}/{}/commits?sha={}&limit={}",
            self.api_base(),
            owner,
            repo,
            urlencoding::encode(branch),
            per_page
        );

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to list commits for {}/{}: {}",
                owner,
                repo,
                response.status()
            )));
        }

        let commits: Vec<GiteaCommit> = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to parse commits: {}", e)))?;

        Ok(commits
            .into_iter()
            .map(|c| {
                let date = chrono::DateTime::parse_from_rfc3339(&c.commit.committer.date)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|_| chrono::Utc::now());
                Commit {
                    sha: c.sha,
                    message: c.commit.message,
                    author: c.commit.author.name,
                    author_email: c.commit.author.email,
                    date,
                }
            })
            .collect())
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
        let url = format!("{}/repos/{}/{}/hooks", self.api_base(), owner, repo);

        #[derive(Serialize)]
        struct GiteaHookConfig {
            url: String,
            content_type: String,
            secret: Option<String>,
        }

        #[derive(Serialize)]
        struct CreateGiteaHookRequest {
            #[serde(rename = "type")]
            hook_type: String,
            config: GiteaHookConfig,
            events: Vec<String>,
            active: bool,
        }

        let mut events = Vec::new();
        if config.events.contains(&"push".to_string()) {
            events.push("push".to_string());
        }
        if config.events.contains(&"pull_request".to_string()) {
            events.push("pull_request".to_string());
        }
        if events.is_empty() {
            events.push("push".to_string()); // default to push
        }

        let request = CreateGiteaHookRequest {
            hook_type: "gitea".to_string(),
            config: GiteaHookConfig {
                url: config.url,
                content_type: "json".to_string(),
                secret: config.secret,
            },
            events,
            active: true,
        };

        let response = self
            .send_with_retry(|| client.post(&url).headers(headers.clone()).json(&request))
            .await?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(GitProviderError::ApiError(format!(
                "Failed to create Gitea webhook for {}/{}: {}",
                owner, repo, text
            )));
        }

        let hook: GiteaHookResponse = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse webhook response: {}", e))
        })?;

        Ok(hook.id.to_string())
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
        let url = format!(
            "{}/repos/{}/{}/hooks/{}",
            self.api_base(),
            owner,
            repo,
            webhook_id
        );

        let response = self
            .send_with_retry(|| client.delete(&url).headers(headers.clone()))
            .await?;

        // 404 means already gone — treat as success (idempotent).
        if response.status().as_u16() == 404 {
            return Ok(());
        }

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(GitProviderError::ApiError(format!(
                "Failed to delete Gitea webhook {} for {}/{}: {}",
                webhook_id, owner, repo, text
            )));
        }

        Ok(())
    }

    /// Verify a Gitea webhook signature.
    ///
    /// Gitea signs payloads with `HMAC-SHA256(key=secret, message=raw_body)`,
    /// transmitting the hex digest in `X-Gitea-Signature`.
    ///
    /// Verification is performed on the raw bytes BEFORE JSON parsing —
    /// the `payload` parameter is the raw request body.
    ///
    /// # Security
    /// - Uses `hmac::Mac::verify_slice` which performs a constant-time comparison.
    /// - Returns `false` on any key/signature parse error rather than propagating.
    async fn verify_webhook_signature(
        &self,
        payload: &[u8],
        signature: &str,
        secret: &str,
    ) -> Result<bool, GitProviderError> {
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;

        // Hex-decode the signature. An empty or malformed hex string rejects.
        let decoded = match hex::decode(signature) {
            Ok(d) => d,
            Err(_) => return Ok(false),
        };

        let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
            Ok(m) => m,
            Err(_) => return Ok(false),
        };
        mac.update(payload);

        // Constant-time comparison via hmac crate.
        Ok(mac.verify_slice(&decoded).is_ok())
    }

    async fn check_repository_accessible(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<bool, GitProviderError> {
        let client = self.get_client();
        let url = format!("{}/repos/{}/{}", self.api_base(), owner, repo);
        let response = self.send_with_retry(|| client.get(&url)).await?;
        Ok(response.status().is_success())
    }

    async fn clone_repository(
        &self,
        clone_url: &str,
        target_dir: &str,
        access_token: Option<&str>,
    ) -> Result<(), GitProviderError> {
        // MUST-FIX 4: validate the clone URL at clone time (not just create time)
        // to prevent a later metadata edit from bypassing the SSRF check.
        temps_core::url_validation::validate_git_url(clone_url).map_err(|e| {
            GitProviderError::InvalidConfiguration(format!(
                "Gitea clone URL failed HTTPS validation: {}",
                e
            ))
        })?;

        let clone_url = clone_url.to_string();
        let target_dir = std::path::PathBuf::from(target_dir);
        let access_token = access_token.map(|s| s.to_string());

        const CLONE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

        let join = tokio::task::spawn_blocking(move || {
            let target = target_dir.as_path();
            if let Some(token) = &access_token {
                // Gitea HTTPS PAT clone uses "x-access-token" as username.
                super::git_ops::clone_repo_with_credentials(
                    &clone_url,
                    target,
                    "x-access-token",
                    token,
                    None,
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
            "Downloading Gitea archive for {}/{} at ref {}",
            owner, repo, ref_spec
        );

        let encoded_ref = urlencoding::encode(ref_spec);
        let url = format!(
            "{}/repos/{}/{}/archive/{}.tar.gz",
            self.api_base(),
            owner,
            repo,
            encoded_ref
        );

        let client = self.get_archive_client();
        let headers = self.get_headers(access_token);

        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        // Follow one redirect hop if the Gitea instance uses a CDN/object store,
        // but only after validating the target host against our base_url domain.
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
                "Gitea archive: following validated redirect to host {}",
                redirect_url.host_str().unwrap_or("?")
            );

            // Do NOT forward the auth token to the redirect target — signed
            // URLs are self-authenticating and forwarding the token to an
            // unknown host would leak it.
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
            let text = response.text().await.unwrap_or_default();
            return Err(GitProviderError::ApiError(format!(
                "Failed to download Gitea archive for {}/{}: {} - {}",
                owner, repo, status, text
            )));
        }

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

        info!("Successfully downloaded Gitea archive to {:?}", target_path);
        Ok(())
    }

    async fn create_source(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        reference: &str,
    ) -> Result<Box<dyn temps_presets::source::ProjectSource>, GitProviderError> {
        Ok(Box::new(crate::sources::GiteaSource::new(
            std::sync::Arc::new(self.get_client()),
            self.base_url.clone(),
            owner.to_string(),
            repo.to_string(),
            reference.to_string(),
            access_token.to_string(),
        )))
    }

    async fn mint_scoped_repo_token(
        &self,
        _access_token: Option<&str>,
        _owner: &str,
        _repo: &str,
        _operation: super::git_provider::ScopedTokenOp,
    ) -> Result<super::git_provider::ScopedTokenGrant, GitProviderError> {
        // Gitea does not support scoped per-repo tokens in v1. The credential
        // daemon falls back to the stored long-lived PAT (same posture as
        // GitLab PAT connections).
        Err(GitProviderError::NotImplemented)
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

        // Choose the endpoint: org repo or user repo.
        let url = if let Some(org) = owner {
            format!("{}/orgs/{}/repos", self.api_base(), org)
        } else {
            format!("{}/user/repos", self.api_base())
        };

        #[derive(Serialize)]
        struct CreateGiteaRepoRequest<'a> {
            name: &'a str,
            description: Option<&'a str>,
            private: bool,
            auto_init: bool,
        }

        let request = CreateGiteaRepoRequest {
            name,
            description,
            private,
            auto_init: true,
        };

        info!("Creating Gitea repository {} (private: {})", name, private);

        let response = self
            .send_with_retry(|| client.post(&url).headers(headers.clone()).json(&request))
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(GitProviderError::ApiError(format!(
                "Failed to create Gitea repository {}: {} - {}",
                name, status, text
            )));
        }

        let gitea_repo: GiteaRepo = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse created repository: {}", e))
        })?;

        Ok(gitea_repo.into_repository())
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
            "Pushing {} files to Gitea {}/{} on branch {}",
            files.len(),
            owner,
            repo,
            branch
        );

        // Gitea creates files one at a time; we iterate and return the last commit.
        let mut last_commit: Option<Commit> = None;

        for (path, content) in files {
            let url = format!(
                "{}/repos/{}/{}/contents/{}",
                self.api_base(),
                owner,
                repo,
                urlencoding::encode(&path)
            );

            let encoded = STANDARD.encode(&content);

            let request_body = serde_json::json!({
                "message": commit_message,
                "content": encoded,
                "branch": branch,
            });

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
                let text = response.text().await.unwrap_or_default();
                return Err(GitProviderError::ApiError(format!(
                    "Failed to push file {} to Gitea {}/{}: {} - {}",
                    path, owner, repo, status, text
                )));
            }

            #[derive(Deserialize)]
            struct CreateFileResponse {
                commit: GiteaCommitSimple,
            }

            #[derive(Deserialize)]
            struct GiteaCommitSimple {
                sha: String,
                message: String,
                author: Option<GiteaCommitAuthor>,
                #[allow(dead_code)]
                committer: Option<GiteaCommitCommitter>,
                created: Option<String>,
            }

            let resp: CreateFileResponse = response.json().await.map_err(|e| {
                GitProviderError::ApiError(format!("Failed to parse file push response: {}", e))
            })?;

            let date = resp
                .commit
                .created
                .as_deref()
                .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(chrono::Utc::now);

            last_commit = Some(Commit {
                sha: resp.commit.sha,
                message: resp.commit.message,
                author: resp.commit.author.map(|a| a.name).unwrap_or_default(),
                author_email: String::new(),
                date,
            });
        }

        last_commit
            .ok_or_else(|| GitProviderError::ApiError("No files were pushed to Gitea".to_string()))
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

        let url = format!("{}/repos/{}/{}/pulls", self.api_base(), owner, repo);

        #[derive(Serialize)]
        struct CreateGiteaPrRequest<'a> {
            title: &'a str,
            body: &'a str,
            head: &'a str,
            base: &'a str,
        }

        let request = CreateGiteaPrRequest {
            title,
            body,
            head: head_branch,
            base: base_branch,
        };

        info!(
            "Creating Gitea pull request '{}' in {}/{}: {} -> {}",
            title, owner, repo, head_branch, base_branch
        );

        let response = self
            .send_with_retry(|| client.post(&url).headers(headers.clone()).json(&request))
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(GitProviderError::ApiError(format!(
                "Failed to create Gitea pull request in {}/{}: {} - {}",
                owner, repo, status, text
            )));
        }

        let pr: GiteaPullRequest = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse pull request response: {}", e))
        })?;

        Ok(PullRequest {
            number: pr.number as i32,
            url: pr.url,
            title: pr.title,
            head_branch: pr.head.label,
            base_branch: pr.base.label,
            head_sha: pr.head_commit_id,
        })
    }
}

// ── Token generation helper ──────────────────────────────────────────────────

/// Generate a random 32-byte hex signing secret for a Gitea webhook.
///
/// Uses `rand::rngs::OsRng` (MUST-FIX 1 — not `thread_rng`, which is
/// deprecated in rand 0.9 and uses a weaker source).
///
/// The hex string is what Gitea stores and uses as the HMAC key for
/// `X-Gitea-Signature: hex(HMAC-SHA256(key=secret, msg=body))`.
pub fn generate_gitea_signing_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_provider(base_url: &str) -> GiteaProvider {
        GiteaProvider::new(
            base_url.to_string(),
            AuthMethod::PersonalAccessToken {
                token: "test-token".to_string(),
            },
        )
    }

    // ── api_base ──────────────────────────────────────────────────────────────

    #[test]
    fn test_api_base_strips_trailing_slash() {
        let p = make_provider("https://git.example.com/");
        assert_eq!(p.api_base(), "https://git.example.com/api/v1");
    }

    #[test]
    fn test_api_base_no_trailing_slash() {
        let p = make_provider("https://git.example.com");
        assert_eq!(p.api_base(), "https://git.example.com/api/v1");
    }

    // ── auth_header_value ────────────────────────────────────────────────────

    #[test]
    fn test_auth_header_uses_token_prefix() {
        let p = make_provider("https://git.example.com");
        assert_eq!(p.auth_header_value("my-pat"), "token my-pat");
    }

    // ── validate_archive_redirect_host ───────────────────────────────────────

    fn check_redirect(base_url: &str, redirect: &str) -> Result<(), GitProviderError> {
        let p = make_provider(base_url);
        p.validate_archive_redirect_host(&reqwest::Url::parse(redirect).unwrap())
    }

    #[test]
    fn test_redirect_allows_same_host() {
        assert!(check_redirect(
            "https://git.example.com",
            "https://git.example.com/archive.tar.gz"
        )
        .is_ok());
    }

    #[test]
    fn test_redirect_allows_subdomain_of_instance() {
        assert!(check_redirect(
            "https://git.example.com",
            "https://storage.git.example.com/blob/archive.tar.gz"
        )
        .is_ok());
    }

    #[test]
    fn test_redirect_rejects_other_host() {
        assert!(check_redirect(
            "https://git.example.com",
            "https://evil.example.com/archive.tar.gz"
        )
        .is_err());
    }

    #[test]
    fn test_redirect_rejects_http() {
        assert!(check_redirect(
            "https://git.example.com",
            "http://git.example.com/archive.tar.gz"
        )
        .is_err());
    }

    #[test]
    fn test_redirect_rejects_metadata_ip() {
        assert!(check_redirect(
            "https://git.example.com",
            "https://169.254.169.254/latest/meta-data"
        )
        .is_err());
    }

    // ── verify_webhook_signature ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_verify_webhook_signature_valid() {
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;

        let secret = "my-secret";
        let payload = b"push event body";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload);
        let sig = hex::encode(mac.finalize().into_bytes());

        let p = make_provider("https://git.example.com");
        assert!(p
            .verify_webhook_signature(payload, &sig, secret)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_verify_webhook_signature_wrong_secret_rejects() {
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;

        let secret = "correct-secret";
        let payload = b"body";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload);
        let sig = hex::encode(mac.finalize().into_bytes());

        let p = make_provider("https://git.example.com");
        assert!(!p
            .verify_webhook_signature(payload, &sig, "wrong-secret")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_verify_webhook_signature_tampered_payload_rejects() {
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;

        let secret = "s";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(b"original");
        let sig = hex::encode(mac.finalize().into_bytes());

        let p = make_provider("https://git.example.com");
        assert!(!p
            .verify_webhook_signature(b"tampered", &sig, secret)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_verify_webhook_signature_invalid_hex_rejects() {
        let p = make_provider("https://git.example.com");
        assert!(!p
            .verify_webhook_signature(b"body", "not-valid-hex!!!", "secret")
            .await
            .unwrap());
    }

    // ── generate_gitea_signing_token ─────────────────────────────────────────

    #[test]
    fn test_generate_gitea_signing_token_is_32_byte_hex() {
        let tok = generate_gitea_signing_token();
        // 32 bytes → 64 hex chars
        assert_eq!(tok.len(), 64);
        assert!(tok.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_gitea_signing_token_is_unique() {
        let t1 = generate_gitea_signing_token();
        let t2 = generate_gitea_signing_token();
        assert_ne!(t1, t2);
    }
}
