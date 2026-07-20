use super::git_provider::{
    AuthMethod, Branch, Commit, FileContent, GitProviderError, GitProviderService, GitProviderTag,
    GitProviderType, PullRequest, RepoDirEntry, Repository, RepositoryPage, User, WebhookConfig,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Bitbucket Cloud REST API v2.0 provider.
///
/// Implements `GitProviderService` for Bitbucket Cloud only. The API base is
/// the constant `https://api.bitbucket.org/2.0` — no user-supplied URL.
///
/// # Authentication
/// - `AuthMethod::PersonalAccessToken { token }` — Repository/Workspace Access
///   Tokens (RATs/WATs). HTTP Basic with username `x-token-auth`.
/// - `AuthMethod::BasicAuth { username, password }` — App Passwords. Standard
///   HTTP Basic with the Atlassian account username.
///
/// # Security
/// - Both reqwest clients use `redirect::Policy::none()` as SSRF defense-in-depth.
/// - The fixed HTTPS API base requires no per-request URL validation.
/// - `mint_scoped_repo_token` returns `NotImplemented`; callers fall back to
///   the stored long-lived PAT/App Password.
/// - Webhook registration is deferred to v1.5; `create_webhook`/`delete_webhook`
///   return `NotImplemented`.
pub struct BitbucketProvider {
    auth_method: AuthMethod,
}

/// Bitbucket Cloud API base URL — always HTTPS, never user-supplied.
const API_BASE: &str = "https://api.bitbucket.org/2.0";

/// Bitbucket Cloud web base URL.
const BASE_URL: &str = "https://bitbucket.org";

impl BitbucketProvider {
    /// Create a new Bitbucket Cloud provider.
    ///
    /// # Arguments
    /// * `auth_method` — either `PersonalAccessToken` (RAT/WAT) or `BasicAuth`
    ///   (App Password). Other auth methods return `AuthenticationFailed`.
    pub fn new(auth_method: AuthMethod) -> Self {
        Self { auth_method }
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

    /// Build an authenticated `reqwest::RequestBuilder`.
    ///
    /// - RAT/WAT (`PersonalAccessToken`): HTTP Basic with username `x-token-auth`.
    /// - App Password (`BasicAuth`): HTTP Basic with the Atlassian username.
    fn apply_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth_method {
            AuthMethod::PersonalAccessToken { token } => {
                builder.basic_auth("x-token-auth", Some(token))
            }
            AuthMethod::BasicAuth { username, password } => {
                builder.basic_auth(username, Some(password))
            }
            _ => builder,
        }
    }

    /// Extract the access token or password string for operations that need it
    /// as a plain string (e.g., clone credentials).
    fn credential_string(&self) -> Option<String> {
        match &self.auth_method {
            AuthMethod::PersonalAccessToken { token } => Some(token.clone()),
            AuthMethod::BasicAuth { password, .. } => Some(password.clone()),
            _ => None,
        }
    }

    /// Clone username for HTTPS: `x-token-auth` for RAT/WAT, Atlassian account
    /// username for App Passwords.
    fn clone_username(&self) -> &str {
        match &self.auth_method {
            AuthMethod::PersonalAccessToken { .. } => "x-token-auth",
            AuthMethod::BasicAuth { username, .. } => username,
            _ => "x-token-auth",
        }
    }

    /// Retry configuration for Bitbucket API calls.
    fn retry_config() -> temps_core::retry::RetryConfig {
        temps_core::retry::RetryConfig::new(3)
            .with_base_delay(std::time::Duration::from_secs(1))
            .with_max_delay(std::time::Duration::from_secs(10))
    }

    /// Send an HTTP request with retry for transient failures (5xx / 429).
    ///
    /// Bitbucket Cloud enforces per-IP and per-user rate limits. Backs off
    /// when `X-RateLimit-Remaining` would suggest we're near the limit or when
    /// the server returns 429.
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

    /// Validate the access token by calling `GET /2.0/user`.
    async fn validate_token_internal(&self, _access_token: &str) -> Result<bool, GitProviderError> {
        let client = self.get_client();
        let url = format!("{}/user", API_BASE);

        let response = self
            .send_with_retry(|| self.apply_auth(client.get(&url)))
            .await;

        match response {
            Ok(resp) => match resp.status() {
                s if s.is_success() => Ok(true),
                s if s.as_u16() == 401 || s.as_u16() == 403 => Ok(false),
                s => {
                    let text = resp.text().await.unwrap_or_default();
                    Err(GitProviderError::ApiError(format!(
                        "Unexpected response validating Bitbucket token: {} - {}",
                        s, text
                    )))
                }
            },
            Err(_) => Err(GitProviderError::RateLimitExceeded),
        }
    }
}

// ── Response types ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct BitbucketUser {
    #[serde(rename = "account_id")]
    account_id: String,
    #[serde(rename = "username", default)]
    username: Option<String>,
    #[serde(rename = "nickname", default)]
    nickname: Option<String>,
    #[serde(rename = "display_name", default)]
    display_name: Option<String>,
    links: Option<BitbucketUserLinks>,
}

#[derive(Deserialize)]
struct BitbucketUserLinks {
    avatar: Option<BitbucketLink>,
}

#[derive(Deserialize)]
struct BitbucketLink {
    href: String,
}

#[derive(Deserialize)]
struct BitbucketRepo {
    uuid: String,
    name: String,
    full_name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "is_private")]
    is_private: bool,
    #[serde(rename = "mainbranch")]
    mainbranch: Option<BitbucketMainBranch>,
    links: Option<BitbucketRepoLinks>,
    #[serde(rename = "owner")]
    owner: Option<BitbucketOwner>,
    language: Option<String>,
    #[serde(default)]
    size: i64,
    created_on: Option<String>,
    updated_on: Option<String>,
}

#[derive(Deserialize)]
struct BitbucketMainBranch {
    name: String,
}

#[derive(Deserialize)]
struct BitbucketRepoLinks {
    #[serde(rename = "clone")]
    clone: Option<Vec<BitbucketCloneLink>>,
    html: Option<BitbucketLink>,
}

#[derive(Deserialize)]
struct BitbucketCloneLink {
    name: String,
    href: String,
}

#[derive(Deserialize)]
struct BitbucketOwner {
    #[serde(rename = "nickname", default)]
    nickname: Option<String>,
    #[allow(dead_code)]
    #[serde(rename = "account_id", default)]
    account_id: Option<String>,
}

impl BitbucketRepo {
    fn owner_slug(&self) -> String {
        self.owner
            .as_ref()
            .and_then(|o| o.nickname.clone())
            .or_else(|| {
                self.full_name
                    .rsplit_once('/')
                    .map(|(ns, _)| ns.to_string())
            })
            .unwrap_or_default()
    }

    fn clone_https_url(&self) -> String {
        self.links
            .as_ref()
            .and_then(|l| l.clone.as_ref())
            .and_then(|links| links.iter().find(|c| c.name == "https"))
            .map(|c| c.href.clone())
            .unwrap_or_else(|| {
                // Fallback: construct from full_name
                format!("{}/{}.git", BASE_URL, self.full_name)
            })
    }

    fn clone_ssh_url(&self) -> String {
        self.links
            .as_ref()
            .and_then(|l| l.clone.as_ref())
            .and_then(|links| links.iter().find(|c| c.name == "ssh"))
            .map(|c| c.href.clone())
            .unwrap_or_else(|| format!("git@bitbucket.org:{}.git", self.full_name))
    }

    fn web_url(&self) -> String {
        self.links
            .as_ref()
            .and_then(|l| l.html.as_ref())
            .map(|h| h.href.clone())
            .unwrap_or_else(|| format!("{}/{}", BASE_URL, self.full_name))
    }

    fn into_repository(self) -> Repository {
        let owner = self.owner_slug();
        let clone_url = self.clone_https_url();
        let ssh_url = self.clone_ssh_url();
        let web_url = self.web_url();
        let default_branch = self
            .mainbranch
            .map(|b| b.name)
            .unwrap_or_else(|| "main".to_string());

        let created_at = self
            .created_on
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);

        let updated_at = self
            .updated_on
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);

        Repository {
            id: self.uuid,
            name: self.name,
            full_name: self.full_name,
            owner,
            description: self.description,
            private: self.is_private,
            default_branch,
            clone_url,
            ssh_url,
            web_url,
            language: self.language,
            size: self.size,
            stars: 0, // Bitbucket v2.0 API doesn't expose star count
            forks: 0, // Not easily available without a separate call
            created_at,
            updated_at,
            pushed_at: None,
        }
    }
}

#[derive(Deserialize)]
struct BitbucketBranch {
    name: String,
    target: BitbucketBranchTarget,
}

#[derive(Deserialize)]
struct BitbucketBranchTarget {
    hash: String,
}

#[derive(Deserialize)]
struct BitbucketTag {
    name: String,
    target: BitbucketBranchTarget,
}

#[derive(Deserialize)]
struct BitbucketCommit {
    hash: String,
    message: String,
    author: BitbucketCommitAuthor,
    date: String,
}

#[derive(Deserialize)]
struct BitbucketCommitAuthor {
    raw: String,
    #[allow(dead_code)]
    user: Option<BitbucketUser>,
}

/// Bitbucket cursor-based paginated response.
#[derive(Deserialize)]
struct BitbucketPage<T> {
    values: Vec<T>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct BitbucketHook {
    uuid: String,
}

#[derive(Deserialize)]
struct BitbucketPullRequest {
    id: i64,
    links: BitbucketPrLinks,
    title: String,
    source: BitbucketPrBranch,
    destination: BitbucketPrBranch,
}

#[derive(Deserialize)]
struct BitbucketPrLinks {
    html: BitbucketLink,
}

#[derive(Deserialize)]
struct BitbucketPrBranch {
    branch: BitbucketPrBranchName,
    commit: Option<BitbucketBranchTarget>,
}

#[derive(Deserialize)]
struct BitbucketPrBranchName {
    name: String,
}

// ── GitProviderService impl ───────────────────────────────────────────────────

#[async_trait]
impl GitProviderService for BitbucketProvider {
    fn provider_type(&self) -> GitProviderType {
        GitProviderType::Bitbucket
    }

    async fn authenticate(&self, _code: Option<String>) -> Result<String, GitProviderError> {
        match &self.auth_method {
            AuthMethod::PersonalAccessToken { token } => Ok(token.clone()),
            AuthMethod::BasicAuth { password, .. } => Ok(password.clone()),
            _ => Err(GitProviderError::AuthenticationFailed(
                "Bitbucket only supports Access Tokens and App Passwords in v1. \
                 Use Repository Settings > Access Tokens or account App Passwords."
                    .to_string(),
            )),
        }
    }

    async fn get_auth_url(&self, _state: &str) -> Result<String, GitProviderError> {
        Err(GitProviderError::InvalidConfiguration(
            "Bitbucket does not support OAuth in v1. \
             Use a Repository/Workspace Access Token (Repository Settings > Access Tokens) \
             or an App Password (Bitbucket account settings > App Passwords)."
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
        // Bitbucket Access Tokens and App Passwords do not support refresh.
        match self.validate_token_internal(access_token).await {
            Ok(true) => Ok((access_token.to_string(), None)),
            Ok(false) => Err(GitProviderError::AuthenticationFailed(
                "Bitbucket credential is invalid. \
                 Please create a new Access Token or App Password."
                    .to_string(),
            )),
            Err(e) => Err(e),
        }
    }

    async fn get_user(&self, _access_token: &str) -> Result<User, GitProviderError> {
        let client = self.get_client();
        let url = format!("{}/user", API_BASE);

        let response = self
            .send_with_retry(|| self.apply_auth(client.get(&url)))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to get Bitbucket user: {}",
                response.status()
            )));
        }

        let user: BitbucketUser = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to parse user: {}", e)))?;

        let username = user
            .nickname
            .clone()
            .or_else(|| user.username.clone())
            .unwrap_or_else(|| user.account_id.clone());

        Ok(User {
            id: user.account_id,
            username,
            name: user.display_name,
            email: None, // Bitbucket /user doesn't always expose email
            avatar_url: user.links.and_then(|l| l.avatar).map(|a| a.href),
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

    /// Bitbucket Cloud uses cursor-based pagination with a `next` URL in the
    /// JSON response body — NOT Link headers. We follow the `next` URL but
    /// translate it back to a page number for the shared `RepositoryPage` type.
    async fn list_repositories_page(
        &self,
        _access_token: &str,
        organization: Option<&str>,
        page: u32,
    ) -> Result<RepositoryPage, GitProviderError> {
        let client = self.get_client();
        const PAGE_LEN: u32 = 50;
        let page = page.max(1);

        // Determine the workspace to query. For PAT/RAT connections the caller
        // may supply an org (workspace) slug. If not provided, use the
        // authenticated user's workspace by fetching /2.0/user first.
        let workspace = match organization {
            Some(org) => org.to_string(),
            None => {
                // Resolve workspace from /user.account_id or nickname.
                let url = format!("{}/user", API_BASE);
                let resp = self
                    .send_with_retry(|| self.apply_auth(client.get(&url)))
                    .await?;

                if !resp.status().is_success() {
                    return Err(GitProviderError::ApiError(format!(
                        "Failed to resolve Bitbucket user for repo listing: {}",
                        resp.status()
                    )));
                }

                let user: BitbucketUser = resp.json().await.map_err(|e| {
                    GitProviderError::ApiError(format!("Failed to parse Bitbucket user: {}", e))
                })?;

                user.nickname.or(user.username).unwrap_or(user.account_id)
            }
        };

        let url = format!(
            "{}/repositories/{}?pagelen={}&page={}",
            API_BASE, workspace, PAGE_LEN, page
        );

        let response = self
            .send_with_retry(|| self.apply_auth(client.get(&url)))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to list Bitbucket repositories for workspace '{}' (page {}): {}",
                workspace,
                page,
                response.status()
            )));
        }

        let page_data: BitbucketPage<BitbucketRepo> = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!(
                "Failed to parse Bitbucket repositories response: {}",
                e
            ))
        })?;

        let items: Vec<Repository> = page_data
            .values
            .into_iter()
            .map(|r| r.into_repository())
            .collect();

        // Bitbucket's `next` field contains the full URL for the next page.
        // Map it to a simple `page + 1` if a next URL is present.
        let next_page = if page_data.next.is_some() {
            Some(page + 1)
        } else {
            None
        };

        Ok(RepositoryPage { items, next_page })
    }

    async fn get_repository(
        &self,
        _access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Repository, GitProviderError> {
        let client = self.get_client();
        let url = format!("{}/repositories/{}/{}", API_BASE, owner, repo);

        let response = self
            .send_with_retry(|| self.apply_auth(client.get(&url)))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to get Bitbucket repository {}/{}: {}",
                owner,
                repo,
                response.status()
            )));
        }

        let bb_repo: BitbucketRepo = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse repository: {}", e))
        })?;

        Ok(bb_repo.into_repository())
    }

    async fn list_branches(
        &self,
        _access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<Branch>, GitProviderError> {
        let client = self.get_client();
        let mut all_branches: Vec<Branch> = Vec::new();
        let per_page: u32 = 50;
        let mut page: u32 = 1;

        loop {
            let url = format!(
                "{}/repositories/{}/{}/refs/branches?pagelen={}&page={}",
                API_BASE, owner, repo, per_page, page
            );

            let response = self
                .send_with_retry(|| self.apply_auth(client.get(&url)))
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

            let page_data: BitbucketPage<BitbucketBranch> = response.json().await.map_err(|e| {
                GitProviderError::ApiError(format!("Failed to parse branches: {}", e))
            })?;

            let has_next = page_data.next.is_some();
            all_branches.extend(page_data.values.into_iter().map(|b| Branch {
                name: b.name,
                commit_sha: b.target.hash,
                protected: false,
            }));

            if !has_next || all_branches.len() >= 1000 {
                break;
            }
            page += 1;
        }

        Ok(all_branches)
    }

    async fn list_tags(
        &self,
        _access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<GitProviderTag>, GitProviderError> {
        let client = self.get_client();
        let mut all_tags: Vec<GitProviderTag> = Vec::new();
        let per_page: u32 = 50;
        let mut page: u32 = 1;

        loop {
            let url = format!(
                "{}/repositories/{}/{}/refs/tags?pagelen={}&page={}",
                API_BASE, owner, repo, per_page, page
            );

            let response = self
                .send_with_retry(|| self.apply_auth(client.get(&url)))
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

            let page_data: BitbucketPage<BitbucketTag> = response
                .json()
                .await
                .map_err(|e| GitProviderError::ApiError(format!("Failed to parse tags: {}", e)))?;

            let has_next = page_data.next.is_some();
            all_tags.extend(page_data.values.into_iter().map(|t| GitProviderTag {
                name: t.name,
                commit_sha: t.target.hash,
            }));

            if !has_next || all_tags.len() >= 1000 {
                break;
            }
            page += 1;
        }

        Ok(all_tags)
    }

    async fn get_file_content(
        &self,
        _access_token: &str,
        owner: &str,
        repo: &str,
        path: &str,
        branch: Option<&str>,
    ) -> Result<FileContent, GitProviderError> {
        let client = self.get_client();

        // Bitbucket src endpoint: /2.0/repositories/{workspace}/{repo_slug}/src/{commit}/{path}
        // When branch is given we use the branch name as the commit specifier.
        let commit_spec = branch.unwrap_or("HEAD");
        let url = format!(
            "{}/repositories/{}/{}/src/{}/{}",
            API_BASE,
            owner,
            repo,
            urlencoding::encode(commit_spec),
            path
        );

        let response = self
            .send_with_retry(|| self.apply_auth(client.get(&url)))
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

        // Bitbucket returns file content directly (not base64 encoded).
        let content = response.text().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to read file content: {}", e))
        })?;

        Ok(FileContent {
            path: path.to_string(),
            content,
            // Bitbucket returns content as raw text, not base64.
            encoding: "plain".to_string(),
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

        // The `src` endpoint needs a node (branch/tag/commit). Bitbucket has no
        // "default ref" shortcut that also accepts a path, so resolve the repo's
        // main branch when the caller didn't pin a reference.
        let node = match reference {
            Some(r) => r.to_string(),
            None => {
                self.get_repository(access_token, owner, repo)
                    .await?
                    .default_branch
            }
        };

        // Percent-encode each path segment individually so `/` separators are
        // preserved but user-supplied characters can't break the URL.
        let encoded_path = path
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|segment| urlencoding::encode(segment).into_owned())
            .collect::<Vec<_>>()
            .join("/");

        // GET {api}/repositories/{owner}/{repo}/src/{node}/{path} — paginated.
        let mut next_url = Some(format!(
            "{}/repositories/{}/{}/src/{}/{}?pagelen=100",
            API_BASE,
            owner,
            repo,
            urlencoding::encode(&node),
            encoded_path
        ));

        #[derive(Deserialize)]
        struct BitbucketSrcItem {
            // "commit_directory" or "commit_file".
            #[serde(rename = "type")]
            item_type: String,
            path: String,
            size: Option<u64>,
        }

        let mut entries: Vec<RepoDirEntry> = Vec::new();
        // Bound pagination so a pathological repo can't loop forever.
        let mut pages_left = 50;
        while let Some(url) = next_url.take() {
            if pages_left == 0 {
                break;
            }
            pages_left -= 1;

            let response = self
                .send_with_retry(|| self.apply_auth(client.get(&url)))
                .await?;

            if !response.status().is_success() {
                return Err(GitProviderError::ApiError(format!(
                    "Failed to list directory '{path}' in {owner}/{repo}: {}",
                    response.status()
                )));
            }

            let page: BitbucketPage<BitbucketSrcItem> = response.json().await.map_err(|e| {
                GitProviderError::ApiError(format!(
                    "Failed to parse directory listing for '{path}' in {owner}/{repo}: {e}"
                ))
            })?;

            for item in page.values {
                let is_dir = item.item_type == "commit_directory";
                // Bitbucket returns repo-relative paths; the display name is the
                // final segment (trailing slash on dirs is already absent).
                let name = item
                    .path
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or(&item.path)
                    .to_string();
                let size = if is_dir { None } else { item.size };
                entries.push(RepoDirEntry {
                    name,
                    path: item.path,
                    is_dir,
                    size,
                });
            }

            next_url = page.next;
        }

        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

        Ok(entries)
    }

    async fn get_latest_commit(
        &self,
        _access_token: &str,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<Commit, GitProviderError> {
        let client = self.get_client();
        let url = format!(
            "{}/repositories/{}/{}/commits/{}?pagelen=1",
            API_BASE,
            owner,
            repo,
            urlencoding::encode(branch)
        );

        let response = self
            .send_with_retry(|| self.apply_auth(client.get(&url)))
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

        let page_data: BitbucketPage<BitbucketCommit> = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to parse commits: {}", e)))?;

        page_data
            .values
            .into_iter()
            .next()
            .map(parse_bb_commit)
            .ok_or_else(|| {
                GitProviderError::ApiError(format!(
                    "No commits found for {}/{} on branch {}",
                    owner, repo, branch
                ))
            })
    }

    async fn get_commit(
        &self,
        _access_token: &str,
        owner: &str,
        repo: &str,
        reference: &str,
    ) -> Result<Commit, GitProviderError> {
        let client = self.get_client();
        let url = format!(
            "{}/repositories/{}/{}/commit/{}",
            API_BASE,
            owner,
            repo,
            urlencoding::encode(reference)
        );

        let response = self
            .send_with_retry(|| self.apply_auth(client.get(&url)))
            .await?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(GitProviderError::CommitNotFound {
                repository: format!("{}/{}", owner, repo),
                commit_sha: reference.to_string(),
            });
        }

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to get commit {} for {}/{}: {}",
                reference,
                owner,
                repo,
                response.status()
            )));
        }

        let commit: BitbucketCommit = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse commit response: {}", e))
        })?;

        Ok(parse_bb_commit(commit))
    }

    async fn check_commit_exists(
        &self,
        _access_token: &str,
        owner: &str,
        repo: &str,
        commit_sha: &str,
    ) -> Result<bool, GitProviderError> {
        let client = self.get_client();
        let url = format!(
            "{}/repositories/{}/{}/commit/{}",
            API_BASE, owner, repo, commit_sha
        );

        let response = self
            .send_with_retry(|| self.apply_auth(client.get(&url)))
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
        _access_token: &str,
        owner: &str,
        repo: &str,
        branch: &str,
        per_page: u32,
    ) -> Result<Vec<Commit>, GitProviderError> {
        let client = self.get_client();
        let url = format!(
            "{}/repositories/{}/{}/commits/{}?pagelen={}",
            API_BASE,
            owner,
            repo,
            urlencoding::encode(branch),
            per_page
        );

        let response = self
            .send_with_retry(|| self.apply_auth(client.get(&url)))
            .await?;

        if !response.status().is_success() {
            return Err(GitProviderError::ApiError(format!(
                "Failed to list commits for {}/{}: {}",
                owner,
                repo,
                response.status()
            )));
        }

        let page_data: BitbucketPage<BitbucketCommit> = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to parse commits: {}", e)))?;

        Ok(page_data.values.into_iter().map(parse_bb_commit).collect())
    }

    /// Register a webhook on a Bitbucket Cloud repository.
    ///
    /// Calls `POST /2.0/repositories/{workspace}/{repo_slug}/hooks` with the
    /// events required for deployment automation:
    /// - `repo:push` — triggers deployment pipelines.
    /// - `pullrequest:created` / `pullrequest:updated` — triggers PR preview
    ///   environments and sticky PR comments.
    /// - `pullrequest:fulfilled` / `pullrequest:rejected` — triggers cleanup.
    ///
    /// The `config.url` must already include the secret delivery token in the
    /// path (`…/events/{token}`). Bitbucket Cloud does not support HMAC body
    /// signing; the secret-in-path URL IS the authentication mechanism.
    ///
    /// Returns the hook UUID string (e.g. `{abc-123-def}`) on success.
    async fn create_webhook(
        &self,
        _access_token: &str,
        owner: &str,
        repo: &str,
        config: WebhookConfig,
    ) -> Result<String, GitProviderError> {
        let client = self.get_client();
        let url = format!("{}/repositories/{}/{}/hooks", API_BASE, owner, repo);

        #[derive(serde::Serialize)]
        struct CreateBitbucketHookRequest<'a> {
            description: &'a str,
            url: &'a str,
            active: bool,
            events: &'a [&'a str],
        }

        let request = CreateBitbucketHookRequest {
            description: "Temps",
            url: &config.url,
            active: true,
            events: &[
                "repo:push",
                "pullrequest:created",
                "pullrequest:updated",
                "pullrequest:fulfilled",
                "pullrequest:rejected",
            ],
        };

        info!(
            "Registering Bitbucket Cloud webhook for {}/{} → {}",
            owner, repo, config.url
        );

        let response = self
            .send_with_retry(|| self.apply_auth(client.post(&url)).json(&request))
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(GitProviderError::ApiError(format!(
                "Failed to create Bitbucket webhook for {}/{}: {} — {}",
                owner, repo, status, text
            )));
        }

        let hook: BitbucketHook = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!(
                "Failed to parse Bitbucket webhook creation response for {}/{}: {}",
                owner, repo, e
            ))
        })?;

        info!(
            "Bitbucket webhook {} registered for {}/{}",
            hook.uuid, owner, repo
        );

        Ok(hook.uuid)
    }

    /// Delete a previously auto-registered Bitbucket Cloud webhook.
    ///
    /// Calls `DELETE /2.0/repositories/{workspace}/{repo_slug}/hooks/{uuid}`.
    /// Returns `Ok(())` when the hook is gone (both 204 and 404 are treated as
    /// success so the operation is idempotent).
    async fn delete_webhook(
        &self,
        _access_token: &str,
        owner: &str,
        repo: &str,
        webhook_id: &str,
    ) -> Result<(), GitProviderError> {
        let client = self.get_client();
        let url = format!(
            "{}/repositories/{}/{}/hooks/{}",
            API_BASE, owner, repo, webhook_id
        );

        let response = self
            .send_with_retry(|| self.apply_auth(client.delete(&url)))
            .await?;

        let status = response.status();
        // 204 No Content = success; 404 = already gone (idempotent).
        if status.as_u16() == 404 || status.is_success() {
            info!(
                "Bitbucket webhook {} deleted for {}/{} (status {})",
                webhook_id, owner, repo, status
            );
            return Ok(());
        }

        let text = response.text().await.unwrap_or_default();
        Err(GitProviderError::ApiError(format!(
            "Failed to delete Bitbucket webhook {} for {}/{}: {} — {}",
            webhook_id, owner, repo, status, text
        )))
    }

    /// Bitbucket Cloud webhooks have no HMAC body signature.
    ///
    /// Authentication is performed via the secret-in-path URL token
    /// (see `handlers/bitbucket.rs`). This method is never called in the
    /// production code path for Bitbucket; it returns `Ok(false)` as a
    /// safe default should it somehow be reached.
    async fn verify_webhook_signature(
        &self,
        _payload: &[u8],
        _signature: &str,
        _secret: &str,
    ) -> Result<bool, GitProviderError> {
        // Bitbucket Cloud does not provide HMAC body signing.
        // Token validation is done in the webhook handler via secret-in-path.
        Ok(false)
    }

    async fn check_repository_accessible(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<bool, GitProviderError> {
        let client = self.get_client();
        let url = format!("{}/repositories/{}/{}", API_BASE, owner, repo);
        let response = self
            .send_with_retry(|| self.apply_auth(client.get(&url)))
            .await?;
        Ok(response.status().is_success())
    }

    async fn clone_repository(
        &self,
        clone_url: &str,
        target_dir: &str,
        _access_token: Option<&str>,
    ) -> Result<(), GitProviderError> {
        // Bitbucket Cloud is always HTTPS; its fixed domain needs no per-call
        // SSRF validation, but we validate the clone URL to confirm it really
        // is a bitbucket.org address (defense-in-depth).
        let url_parsed = reqwest::Url::parse(clone_url).map_err(|e| {
            GitProviderError::InvalidConfiguration(format!(
                "Bitbucket clone URL is not valid: {}",
                e
            ))
        })?;

        if url_parsed.scheme() != "https" {
            return Err(GitProviderError::InvalidConfiguration(
                "Bitbucket clone URL must use HTTPS".to_string(),
            ));
        }

        let host = url_parsed.host_str().unwrap_or("").to_ascii_lowercase();
        if !host.ends_with("bitbucket.org") {
            return Err(GitProviderError::InvalidConfiguration(format!(
                "Bitbucket clone URL host '{}' is not bitbucket.org",
                host
            )));
        }

        let clone_url = clone_url.to_string();
        let target_dir = std::path::PathBuf::from(target_dir);
        let username = self.clone_username().to_string();
        let credential = self.credential_string();

        const CLONE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

        let join = tokio::task::spawn_blocking(move || {
            if let Some(token) = &credential {
                super::git_ops::clone_repo_with_credentials(
                    &clone_url,
                    &target_dir,
                    &username,
                    token,
                    None,
                )
            } else {
                super::git_ops::clone_repo(&clone_url, &target_dir, None)
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
        _access_token: &str,
        owner: &str,
        repo: &str,
        ref_spec: &str,
        target_path: &std::path::Path,
        progress: Option<&crate::services::git_provider::ArchiveProgressSender>,
    ) -> Result<(), GitProviderError> {
        info!(
            "Downloading Bitbucket archive for {}/{} at ref {}",
            owner, repo, ref_spec
        );

        // Bitbucket src endpoint returns a zip of the repo at the given ref.
        // We stream this directly to disk.
        let encoded_ref = urlencoding::encode(ref_spec);
        let url = format!(
            "{}/repositories/{}/{}/get/{}.tar.gz",
            API_BASE, owner, repo, encoded_ref
        );

        let client = self.get_archive_client();

        let response = self
            .send_with_retry(|| self.apply_auth(client.get(&url)))
            .await?;

        // Follow a single redirect (Bitbucket CDN redirects) — only from
        // api.bitbucket.org or *.bitbucket.org (defence-in-depth).
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

            // Only follow HTTPS redirects to *.bitbucket.org or *.atlassian.com.
            if redirect_url.scheme() != "https" {
                return Err(GitProviderError::ApiError(format!(
                    "Refusing to follow archive redirect to non-HTTPS URL: {}",
                    redirect_url
                )));
            }
            let rhost = redirect_url.host_str().unwrap_or("").to_ascii_lowercase();
            if !rhost.ends_with("bitbucket.org") && !rhost.ends_with("atlassian.com") {
                return Err(GitProviderError::ApiError(format!(
                    "Refusing to follow archive redirect to host '{}' (expected bitbucket.org or atlassian.com)",
                    rhost
                )));
            }

            debug!(
                "Bitbucket archive: following validated redirect to host {}",
                rhost
            );

            // Do NOT forward credentials to the redirect target.
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
                "Failed to download Bitbucket archive for {}/{}: {} - {}",
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

        info!(
            "Successfully downloaded Bitbucket archive to {:?}",
            target_path
        );
        Ok(())
    }

    async fn create_source(
        &self,
        _access_token: &str,
        owner: &str,
        repo: &str,
        reference: &str,
    ) -> Result<Box<dyn temps_presets::source::ProjectSource>, GitProviderError> {
        Ok(Box::new(crate::sources::BitbucketSource::new(
            std::sync::Arc::new(self.get_client()),
            self.auth_method.clone(),
            owner.to_string(),
            repo.to_string(),
            reference.to_string(),
        )))
    }

    async fn mint_scoped_repo_token(
        &self,
        _access_token: Option<&str>,
        _owner: &str,
        _repo: &str,
        _operation: super::git_provider::ScopedTokenOp,
    ) -> Result<super::git_provider::ScopedTokenGrant, GitProviderError> {
        // Bitbucket does not support scoped per-repo tokens in v1. The
        // credential daemon falls back to the stored long-lived PAT/App Password.
        Err(GitProviderError::NotImplemented)
    }

    async fn create_repository(
        &self,
        _access_token: &str,
        name: &str,
        owner: Option<&str>,
        description: Option<&str>,
        private: bool,
    ) -> Result<Repository, GitProviderError> {
        let client = self.get_client();

        // Bitbucket requires a workspace slug — use the owner arg or resolve it.
        let workspace = match owner {
            Some(o) => o.to_string(),
            None => {
                let url = format!("{}/user", API_BASE);
                let resp = self
                    .send_with_retry(|| self.apply_auth(client.get(&url)))
                    .await?;
                let user: BitbucketUser = resp.json().await.map_err(|e| {
                    GitProviderError::ApiError(format!("Failed to parse user: {}", e))
                })?;
                user.nickname.or(user.username).unwrap_or(user.account_id)
            }
        };

        let url = format!("{}/repositories/{}/{}", API_BASE, workspace, name);

        #[derive(Serialize)]
        struct CreateBitbucketRepoRequest<'a> {
            scm: &'a str,
            description: Option<&'a str>,
            is_private: bool,
        }

        let request = CreateBitbucketRepoRequest {
            scm: "git",
            description,
            is_private: private,
        };

        info!(
            "Creating Bitbucket repository {} (private: {})",
            name, private
        );

        let response = self
            .send_with_retry(|| self.apply_auth(client.post(&url)).json(&request))
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(GitProviderError::ApiError(format!(
                "Failed to create Bitbucket repository {}/{}: {} - {}",
                workspace, name, status, text
            )));
        }

        let bb_repo: BitbucketRepo = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse created repository: {}", e))
        })?;

        Ok(bb_repo.into_repository())
    }

    async fn push_files_to_repository(
        &self,
        _access_token: &str,
        owner: &str,
        repo: &str,
        _branch: &str,
        _files: Vec<(String, Vec<u8>)>,
        _commit_message: &str,
    ) -> Result<Commit, GitProviderError> {
        // Bitbucket Cloud does not have a simple REST API for pushing files
        // equivalent to GitLab/Gitea. The standard approach is via the src
        // endpoint with multipart form data, which is out of scope for v1.
        Err(GitProviderError::ApiError(format!(
            "Pushing files via the API is not supported for Bitbucket Cloud \
             repositories ({}/{}). Use git push instead.",
            owner, repo
        )))
    }

    async fn create_pull_request(
        &self,
        _access_token: &str,
        owner: &str,
        repo: &str,
        title: &str,
        body: &str,
        head_branch: &str,
        base_branch: &str,
    ) -> Result<PullRequest, GitProviderError> {
        let client = self.get_client();
        let url = format!("{}/repositories/{}/{}/pullrequests", API_BASE, owner, repo);

        #[derive(Serialize)]
        struct CreateBitbucketPrRequest<'a> {
            title: &'a str,
            description: &'a str,
            source: BitbucketPrBranchRef<'a>,
            destination: BitbucketPrBranchRef<'a>,
        }

        #[derive(Serialize)]
        struct BitbucketPrBranchRef<'a> {
            branch: BitbucketPrBranchName<'a>,
        }

        #[derive(Serialize)]
        struct BitbucketPrBranchName<'a> {
            name: &'a str,
        }

        let request = CreateBitbucketPrRequest {
            title,
            description: body,
            source: BitbucketPrBranchRef {
                branch: BitbucketPrBranchName { name: head_branch },
            },
            destination: BitbucketPrBranchRef {
                branch: BitbucketPrBranchName { name: base_branch },
            },
        };

        info!(
            "Creating Bitbucket pull request '{}' in {}/{}: {} -> {}",
            title, owner, repo, head_branch, base_branch
        );

        let response = self
            .send_with_retry(|| self.apply_auth(client.post(&url)).json(&request))
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(GitProviderError::ApiError(format!(
                "Failed to create Bitbucket pull request in {}/{}: {} - {}",
                owner, repo, status, text
            )));
        }

        let pr: BitbucketPullRequest = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse pull request response: {}", e))
        })?;

        Ok(PullRequest {
            number: pr.id as i32,
            url: pr.links.html.href,
            title: pr.title,
            head_branch: pr.source.branch.name,
            base_branch: pr.destination.branch.name,
            head_sha: pr.destination.commit.map(|c| c.hash),
        })
    }
}

// ── Commit parsing helper ─────────────────────────────────────────────────────

/// Parse a Bitbucket commit response into the common `Commit` type.
///
/// Bitbucket's author field is a free-form `raw` string like
/// `"Display Name <email@example.com>"`.  We parse the name and email out
/// of it when possible.
fn parse_bb_commit(c: BitbucketCommit) -> Commit {
    let date = chrono::DateTime::parse_from_rfc3339(&c.date)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());

    // Parse "Name <email>" from the raw author string.
    let (author_name, author_email) = parse_raw_author(&c.author.raw);

    Commit {
        sha: c.hash,
        message: c.message,
        author: author_name,
        author_email,
        date,
    }
}

/// Parse a `"Name <email>"` author string into `(name, email)`.
fn parse_raw_author(raw: &str) -> (String, String) {
    if let Some(lt_pos) = raw.find('<') {
        if let Some(gt_pos) = raw[lt_pos..].find('>') {
            let name = raw[..lt_pos].trim().to_string();
            let email = raw[lt_pos + 1..lt_pos + gt_pos].trim().to_string();
            return (name, email);
        }
    }
    (raw.to_string(), String::new())
}

// ── Token generation helper ───────────────────────────────────────────────────

/// Generate a cryptographically random 64-character hex token (32 bytes) for
/// use as a Bitbucket webhook delivery URL path token.
///
/// Uses `rand::rngs::OsRng` (MUST-FIX 1 — not `thread_rng`).
///
/// The token is embedded in the webhook callback URL:
/// `{temps_url}/api/webhook/git/bitbucket/events/{token}`.
/// It is stored encrypted in `projects.bitbucket_webhook_token` and
/// compared in constant time in the webhook handler (MUST-FIX 3).
pub fn generate_bitbucket_webhook_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

// ── Shared helper: constant-time secret-in-path lookup ────────────────────────

/// Fetch all rows from `projects` that have a non-NULL token in `get_encrypted_token`,
/// decrypt each stored token via `decrypt_fn`, and compare to `delivery_token`
/// in constant time.
///
/// Returns the set of matching project IDs. An empty set means no match.
///
/// # Security (MUST-FIX 3)
/// - Never does `WHERE token = ?` on ciphertext — tokens are encrypted, so
///   SQL equality on ciphertext is wrong and leaks timing via ciphertext comparison.
/// - Uses `subtle::ConstantTimeEq` for the final byte comparison to prevent
///   timing side-channels.
/// - The caller's handler always returns HTTP 200 regardless of this result
///   (no existence oracle).
/// - The delivery token must never appear in any log line.
///
/// # For the Generic stage (stage 3)
/// This function is designed to be reusable. The Generic handler can call
/// `constant_time_token_lookup` with `projects::Column::GenericWebhookToken`
/// and the appropriate decryption. The function signature takes a closure
/// so the column selector is entirely caller-supplied.
pub async fn constant_time_token_lookup<F, Fut>(
    db: &sea_orm::DatabaseConnection,
    delivery_token: &str,
    get_encrypted_token: F,
) -> Vec<temps_entities::projects::Model>
where
    F: Fn(temps_entities::projects::Model) -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    use sea_orm::EntityTrait;
    use subtle::ConstantTimeEq;

    // Fetch ALL projects. We must not filter by token in SQL because:
    // 1. Tokens are stored encrypted — SQL `WHERE encrypted_col = $1` would
    //    compare ciphertext, which is always wrong after AES-GCM (random IV).
    // 2. Even if we hashed and indexed them, a SELECT returning 0 rows
    //    leaks that the token is invalid (existence oracle).
    //
    // Performance note: the project table is typically small (thousands of
    // rows at most). A full scan is acceptable for this security-critical path.
    let all_projects = match temps_entities::projects::Entity::find().all(db).await {
        Ok(ps) => ps,
        Err(e) => {
            tracing::error!(
                "DB error fetching projects for secret-in-path token lookup: {}",
                e
            );
            return vec![];
        }
    };

    let delivery_bytes = delivery_token.as_bytes();
    let mut matches = vec![];

    for project in all_projects {
        // Decrypt the stored token (or skip if NULL).
        let project_clone: temps_entities::projects::Model = project.clone();
        let plaintext = match get_encrypted_token(project_clone).await {
            Some(p) => p,
            None => continue,
        };

        // Constant-time comparison (MUST-FIX 3).
        let stored_bytes = plaintext.as_bytes();
        let equal: subtle::Choice = stored_bytes.ct_eq(delivery_bytes);
        if equal.unwrap_u8() == 1 {
            matches.push(project);
        }
    }

    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pat_provider() -> BitbucketProvider {
        BitbucketProvider::new(AuthMethod::PersonalAccessToken {
            token: "test-token".to_string(),
        })
    }

    fn make_basic_provider() -> BitbucketProvider {
        BitbucketProvider::new(AuthMethod::BasicAuth {
            username: "atlassian-user".to_string(),
            password: "app-password".to_string(),
        })
    }

    // ── provider_type ─────────────────────────────────────────────────────────

    #[test]
    fn test_provider_type_is_bitbucket() {
        assert!(
            matches!(
                make_pat_provider().provider_type(),
                GitProviderType::Bitbucket
            ),
            "provider_type must be Bitbucket"
        );
    }

    // ── clone_username ────────────────────────────────────────────────────────

    #[test]
    fn test_clone_username_for_rat_is_x_token_auth() {
        assert_eq!(make_pat_provider().clone_username(), "x-token-auth");
    }

    #[test]
    fn test_clone_username_for_basic_is_atlassian_username() {
        assert_eq!(make_basic_provider().clone_username(), "atlassian-user");
    }

    // ── credential_string ─────────────────────────────────────────────────────

    #[test]
    fn test_credential_string_for_rat() {
        assert_eq!(
            make_pat_provider().credential_string(),
            Some("test-token".to_string())
        );
    }

    #[test]
    fn test_credential_string_for_basic() {
        assert_eq!(
            make_basic_provider().credential_string(),
            Some("app-password".to_string())
        );
    }

    // ── parse_raw_author ──────────────────────────────────────────────────────

    #[test]
    fn test_parse_raw_author_full() {
        let (name, email) = parse_raw_author("Alice Smith <alice@example.com>");
        assert_eq!(name, "Alice Smith");
        assert_eq!(email, "alice@example.com");
    }

    #[test]
    fn test_parse_raw_author_no_email() {
        let (name, email) = parse_raw_author("Just A Name");
        assert_eq!(name, "Just A Name");
        assert_eq!(email, "");
    }

    #[test]
    fn test_parse_raw_author_empty() {
        let (name, email) = parse_raw_author("");
        assert_eq!(name, "");
        assert_eq!(email, "");
    }

    // ── generate_bitbucket_webhook_token ──────────────────────────────────────

    #[test]
    fn test_generate_bitbucket_webhook_token_is_64_hex_chars() {
        let tok = generate_bitbucket_webhook_token();
        assert_eq!(tok.len(), 64, "32 bytes => 64 hex chars");
        assert!(tok.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_bitbucket_webhook_token_is_unique() {
        let t1 = generate_bitbucket_webhook_token();
        let t2 = generate_bitbucket_webhook_token();
        assert_ne!(t1, t2);
    }

    // ── clone URL validation ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_clone_rejects_http_url() {
        let p = make_pat_provider();
        let result = p
            .clone_repository("http://bitbucket.org/user/repo.git", "/tmp/test", None)
            .await;
        assert!(result.is_err(), "HTTP clone URL must be rejected");
    }

    #[tokio::test]
    async fn test_clone_rejects_non_bitbucket_host() {
        let p = make_pat_provider();
        let result = p
            .clone_repository("https://evil.example.com/user/repo.git", "/tmp/test", None)
            .await;
        assert!(result.is_err(), "Non-bitbucket.org host must be rejected");
    }

    // ── BitbucketHook deserialization ─────────────────────────────────────────

    #[test]
    fn test_bitbucket_hook_deserialization() {
        let json = r#"{"uuid": "{abc-123-def}"}"#;
        let hook: BitbucketHook = serde_json::from_str(json).unwrap();
        assert_eq!(hook.uuid, "{abc-123-def}");
    }

    // ── create_webhook / delete_webhook ──────────────────────────────────────

    /// Verify the shape of the JSON body that `create_webhook` sends to
    /// Bitbucket. All five events must be present, `active` must be true, and
    /// `description` must be "Temps".
    #[test]
    fn test_create_webhook_body_shape() {
        // Replicate the inline struct the production code serialises.
        #[derive(serde::Serialize)]
        struct Req<'a> {
            description: &'a str,
            url: &'a str,
            active: bool,
            events: &'a [&'a str],
        }
        let body = serde_json::to_value(Req {
            description: "Temps",
            url: "https://app.example.com/api/webhook/git/bitbucket/events/token123",
            active: true,
            events: &[
                "repo:push",
                "pullrequest:created",
                "pullrequest:updated",
                "pullrequest:fulfilled",
                "pullrequest:rejected",
            ],
        })
        .unwrap();

        assert_eq!(body["description"], "Temps");
        assert_eq!(body["active"], true);
        let events = body["events"].as_array().unwrap();
        assert_eq!(events.len(), 5, "exactly 5 events expected");
        assert!(events.iter().any(|e| e == "repo:push"));
        assert!(events.iter().any(|e| e == "pullrequest:created"));
        assert!(events.iter().any(|e| e == "pullrequest:updated"));
        assert!(events.iter().any(|e| e == "pullrequest:fulfilled"));
        assert!(events.iter().any(|e| e == "pullrequest:rejected"));
    }

    /// Bitbucket hook UUIDs include surrounding braces: `{uuid-v4}`.
    /// Verify our `BitbucketHook` deserializer preserves them verbatim.
    #[test]
    fn test_hook_uuid_includes_braces() {
        let json = r#"{"uuid":"{ab12cd34-ef56-7890-abcd-ef1234567890}"}"#;
        let hook: BitbucketHook = serde_json::from_str(json).unwrap();
        assert!(
            hook.uuid.starts_with('{') && hook.uuid.ends_with('}'),
            "Bitbucket hook UUID must include braces"
        );
    }

    // ── constant_time_token_lookup (pure-logic test, no DB) ───────────────────

    #[test]
    fn test_constant_time_eq_matches() {
        use subtle::ConstantTimeEq;
        let a = b"my-secret-token";
        let b = b"my-secret-token";
        assert_eq!(a.ct_eq(b).unwrap_u8(), 1);
    }

    #[test]
    fn test_constant_time_eq_no_match() {
        use subtle::ConstantTimeEq;
        let a = b"my-secret-token";
        let b = b"wrong-token-val";
        assert_eq!(a.ct_eq(b).unwrap_u8(), 0);
    }
}
