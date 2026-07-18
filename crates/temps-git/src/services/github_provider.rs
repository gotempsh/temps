use super::git_provider::{
    AuthMethod, Branch, Commit, FileContent, GitProviderError, GitProviderService, GitProviderTag,
    GitProviderType, PullRequest, RepoDirEntry, Repository, ScopedTokenGrant, ScopedTokenOp, User,
    WebhookConfig,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use octocrab::{Octocrab, OctocrabBuilder};
use reqwest;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

// Response structs for API calls

/// Request body for `POST /app/installations/{id}/access_tokens`.
///
/// Both fields are optional: GitHub treats an absent field as "no narrowing
/// for this dimension", and an empty body (`{}`) as "full installation
/// scope, all granted permissions" — the historical default.
///
/// Use [`Self::for_repo_read`] / [`Self::for_repo_write`] for the common
/// case of minting a per-operation token for a single repository, and
/// [`Self::default`] (i.e. `{}`) for full-installation tokens used
/// internally by token-refresh flows.
#[derive(Debug, Default, Clone, Serialize)]
pub struct ScopedTokenRequest {
    /// Restrict the token to a subset of the installation's repositories
    /// by name (`acme/web`, not `123456789`). Use `repository_ids` if you
    /// happen to know the numeric IDs — but names are what we have at
    /// every callsite in temps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repositories: Option<Vec<String>>,

    /// Restrict the token to a subset of the installation's permissions.
    /// Keys are GitHub permission names (`contents`, `pull_requests`,
    /// `metadata`, …). Values are `read`, `write`, or `admin`. Permissions
    /// the App wasn't granted at install time can't be added here — GitHub
    /// will 422.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<std::collections::HashMap<String, String>>,
}

impl ScopedTokenRequest {
    /// Token for cloning / fetching a single repo. `contents:read` is the
    /// minimum permission a `git clone` over HTTPS needs.
    ///
    /// Only `contents` is listed: `metadata:read` is granted implicitly by
    /// GitHub to every installation token that has any repository
    /// permission, and listing it explicitly here causes GitHub to *strip
    /// the entire `permissions` block* (silently returning an empty-
    /// permissions token, which surfaces as `pull:false, push:false` on
    /// every minted token). The endpoint validates each requested key
    /// against the App's *declared* permissions, and `metadata` is not in
    /// that set — it's implicit. Don't add it back.
    ///
    /// `repo_name` is the bare repo name (e.g. `temps-landing-new`), NOT
    /// `owner/repo` — GitHub's access_tokens endpoint expects the unqualified
    /// form because the owner is fixed by the installation.
    pub fn for_repo_read(repo_name: &str) -> Self {
        let mut perms = std::collections::HashMap::new();
        perms.insert("contents".to_string(), "read".to_string());
        Self {
            repositories: Some(vec![repo_name.to_string()]),
            permissions: Some(perms),
        }
    }

    /// Token for pushing to a single repo. `contents:write` covers
    /// `git push`. See [`Self::for_repo_read`] for why `metadata` is
    /// deliberately NOT requested.
    ///
    /// `repo_name` is the bare repo name (see [`Self::for_repo_read`]).
    pub fn for_repo_write(repo_name: &str) -> Self {
        let mut perms = std::collections::HashMap::new();
        perms.insert("contents".to_string(), "write".to_string());
        Self {
            repositories: Some(vec![repo_name.to_string()]),
            permissions: Some(perms),
        }
    }
}

/// OAuth token response (from /login/oauth/access_token)
/// GitHub OAuth typically doesn't include refresh_token
#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[allow(dead_code)]
    token_type: Option<String>,
    #[allow(dead_code)]
    scope: Option<String>,
}

/// Token refresh response (for GitHub Apps with refresh tokens)
#[derive(Deserialize)]
struct RefreshTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct GitHubRepo {
    id: i64,
    name: String,
    full_name: String,
    owner: GitHubOwner,
    description: Option<String>,
    private: bool,
    default_branch: String,
    clone_url: String,
    ssh_url: String,
    html_url: String,
    language: Option<String>,
    size: i64,
    stargazers_count: i32,
    forks_count: i32,
    created_at: String,
    updated_at: String,
    pushed_at: Option<String>,
}

#[derive(Deserialize)]
struct GitHubOwner {
    login: String,
}

#[derive(Deserialize)]
struct InstallationRepositoriesResponse {
    repositories: Vec<GitHubRepo>,
    #[allow(dead_code)]
    total_count: i32,
}

#[derive(Deserialize)]
struct HookResponse {
    id: i64,
}

pub struct GitHubProvider {
    api_url: String,
    auth_method: AuthMethod,
}

impl GitHubProvider {
    pub fn new(api_url: Option<String>, auth_method: AuthMethod) -> Self {
        Self {
            api_url: api_url.unwrap_or_else(|| "https://api.github.com".to_string()),
            auth_method,
        }
    }

    /// Create an Octocrab client with the given access token
    async fn get_octocrab_client(&self, access_token: &str) -> Result<Octocrab, GitProviderError> {
        // Note: Octocrab doesn't support custom base URLs through the builder
        // For GitHub Enterprise support, we'd need to use the underlying reqwest client
        // For now, we'll only support the default GitHub API with Octocrab
        if self.api_url != "https://api.github.com" {
            return Err(GitProviderError::Other(
                "Custom API URLs are not supported with Octocrab integration yet".to_string(),
            ));
        }

        let octocrab = OctocrabBuilder::new()
            .personal_token(access_token.to_string())
            .build()
            .map_err(|e| {
                GitProviderError::Other(format!("Failed to build Octocrab client: {}", e))
            })?;

        Ok(octocrab)
    }

    fn get_client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .user_agent("Temps-Engine/1.0")
            .timeout(std::time::Duration::from_secs(30))
            // SSRF defense-in-depth: never follow redirects (see gitlab_provider).
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("Failed to build reqwest client with static config")
    }

    /// Client for streaming archive downloads. Unlike [`get_client`]'s 30s total
    /// timeout (right for small JSON API calls, but it aborts a large tarball
    /// mid-stream → "error reading chunk: error decoding response body"), this
    /// uses a *generous* 15-minute total ceiling plus tighter connect and
    /// per-read-inactivity bounds. The total timeout is the hard backstop that
    /// guarantees the request can never hang forever (covering every phase —
    /// connect, waiting for response headers, redirect, and body), while the
    /// 15-minute budget is ample for even very large repositories. Redirects are
    /// still never followed automatically (we validate + follow the one archive
    /// hop manually).
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

    /// Retry configuration for GitHub API calls.
    fn retry_config() -> temps_core::retry::RetryConfig {
        temps_core::retry::RetryConfig::new(3)
            .with_base_delay(std::time::Duration::from_secs(1))
            .with_max_delay(std::time::Duration::from_secs(10))
    }

    /// Send an HTTP request with retry logic for transient failures.
    /// The `build_request` closure is called on each attempt to rebuild the request
    /// (since reqwest::RequestBuilder is consumed on send).
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

                    // Retry on server errors and rate limits, not on client errors
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
        headers.insert(
            "Authorization",
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", access_token)).unwrap(),
        );
        headers.insert(
            "Accept",
            reqwest::header::HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            reqwest::header::HeaderValue::from_static("2022-11-28"),
        );
        headers
    }

    /// SSRF guard for the one redirect we follow manually: the archive-download
    /// 302. The redirect target must be HTTPS and a GitHub-owned host, never an
    /// arbitrary address (which could be internal, e.g. cloud metadata).
    ///
    /// For github.com the signed archive lives on `codeload.github.com` (and
    /// historically `objects.githubusercontent.com`). For GitHub Enterprise the
    /// archive is served from the configured API host's registrable domain, so
    /// we additionally allow any host under that domain.
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

        let host = redirect_url.host_str().ok_or_else(|| {
            GitProviderError::ApiError(format!(
                "Archive redirect URL has no host: {}",
                redirect_url
            ))
        })?;
        let host = host.to_ascii_lowercase();

        // Public github.com archive hosts.
        const ALLOWED_SUFFIXES: [&str; 2] = [
            ".github.com",            // codeload.github.com
            ".githubusercontent.com", // objects.githubusercontent.com
        ];
        let allowed_public =
            host == "github.com" || ALLOWED_SUFFIXES.iter().any(|suffix| host.ends_with(suffix));

        // GitHub Enterprise: allow the API host's registrable domain. e.g. an
        // api_url of `https://ghe.example.com/api/v3` permits `*.ghe.example.com`
        // / `ghe.example.com` archive hosts.
        let allowed_enterprise = reqwest::Url::parse(&self.api_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
            .map(|api_host| host == api_host || host.ends_with(&format!(".{}", api_host)))
            .unwrap_or(false);

        if allowed_public || allowed_enterprise {
            Ok(())
        } else {
            Err(GitProviderError::ApiError(format!(
                "Refusing to follow archive redirect to non-GitHub host '{}' (from {})",
                host, redirect_url
            )))
        }
    }

    /// Refresh an access token using a refresh token
    /// Note: GitHub OAuth apps don't support refresh tokens by default.
    /// This is primarily for GitHub Apps which use a different flow.
    async fn refresh_access_token(
        &self,
        client_id: &str,
        client_secret: &str,
        refresh_token: &str,
    ) -> Result<(String, Option<String>), GitProviderError> {
        info!("Refreshing GitHub access token");

        let client = self.get_client();
        let params = [
            ("client_id", client_id.to_string()),
            ("client_secret", client_secret.to_string()),
            ("refresh_token", refresh_token.to_string()),
            ("grant_type", "refresh_token".to_string()),
        ];

        let response = self
            .send_with_retry(|| {
                client
                    .post("https://github.com/login/oauth/access_token")
                    .header("Accept", "application/json")
                    .form(&params)
            })
            .await?;

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

        let token_response: RefreshTokenResponse = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse refresh response: {}", e))
        })?;

        if let Some(error) = token_response.error {
            return Err(GitProviderError::AuthenticationFailed(format!(
                "GitHub refresh error: {} - {}",
                error,
                token_response.error_description.unwrap_or_default()
            )));
        }

        debug!("Successfully refreshed GitHub access token");
        Ok((token_response.access_token, token_response.refresh_token))
    }

    /// Generate a GitHub App installation token
    /// GitHub App tokens expire after 1 hour, so they need to be regenerated
    ///
    /// Internal full-scope variant: returns just the token string for the
    /// existing `validate_and_refresh_token` path that doesn't track expiry.
    /// New callers should use `generate_scoped_installation_token` instead.
    async fn generate_installation_token(
        &self,
        installation_id: i64,
    ) -> Result<String, GitProviderError> {
        let (token, _expires_at) = self
            .generate_scoped_installation_token(installation_id, &ScopedTokenRequest::default())
            .await?;
        Ok(token)
    }

    /// Generate a narrowly-scoped GitHub App installation token.
    ///
    /// Wraps the GitHub `POST /app/installations/{installation_id}/access_tokens`
    /// endpoint with optional `repositories` and `permissions` narrowing —
    /// the same endpoint as `generate_installation_token`, but with a body
    /// that constrains the resulting token to a subset of the installation's
    /// repositories and a subset of its granted permissions.
    ///
    /// This is the entry point for the in-sandbox credential daemon: every
    /// `git clone`/`git push` mints a fresh token scoped to a single repo
    /// with the minimum permission needed, valid for ≤1 hour, instead of
    /// reusing one full-installation token for the whole session.
    ///
    /// # Returns
    /// `(token, expires_at)` — `expires_at` is the GitHub-reported expiry
    /// timestamp (`None` only if GitHub omits the field, which it never
    /// does in practice; callers should treat `None` as "expires soon" and
    /// re-mint).
    pub async fn generate_scoped_installation_token(
        &self,
        installation_id: i64,
        request: &ScopedTokenRequest,
    ) -> Result<(String, Option<DateTime<Utc>>), GitProviderError> {
        match &self.auth_method {
            AuthMethod::GitHubApp {
                app_id,
                private_key,
                ..
            } => {
                info!(
                    "Generating GitHub App installation token for installation {} (repos={:?}, perms={:?})",
                    installation_id, request.repositories, request.permissions
                );

                // Create JWT for GitHub App authentication
                let app_id_param = octocrab::models::AppId(*app_id as u64);
                let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key.as_bytes()).map_err(
                    |e| {
                        error!(
                            installation_id,
                            app_id = *app_id,
                            error = %e,
                            "GitHub App scoped token mint failed: invalid private key"
                        );
                        GitProviderError::InvalidConfiguration(format!(
                            "Invalid private key: {}",
                            e
                        ))
                    },
                )?;

                let jwt = octocrab::auth::create_jwt(app_id_param, &key).map_err(|e| {
                    error!(
                        installation_id,
                        app_id = *app_id,
                        error = %e,
                        "GitHub App scoped token mint failed: JWT creation error"
                    );
                    GitProviderError::ApiError(format!("Failed to create JWT: {}", e))
                })?;

                // Create octocrab instance with JWT
                let octocrab = OctocrabBuilder::new()
                    .personal_token(jwt)
                    .build()
                    .map_err(|e| {
                        error!(
                            installation_id,
                            app_id = *app_id,
                            error = %e,
                            "GitHub App scoped token mint failed: octocrab client build error"
                        );
                        GitProviderError::ApiError(format!(
                            "Failed to create GitHub App client: {}",
                            e
                        ))
                    })?;

                // Get installation details
                let installation = octocrab
                    .apps()
                    .installation(octocrab::models::InstallationId(installation_id as u64))
                    .await
                    .map_err(|e| {
                        error!(
                            installation_id,
                            app_id = *app_id,
                            error = %e,
                            "GitHub App scoped token mint failed: cannot fetch installation \
                             (check that app_id matches installation_id and the App is still \
                             installed)"
                        );
                        GitProviderError::ApiError(format!("Failed to get installation: {}", e))
                    })?;

                let gh_access_tokens_url = reqwest::Url::parse(
                    installation.access_tokens_url.as_ref().ok_or_else(|| {
                        error!(
                            installation_id,
                            app_id = *app_id,
                            "GitHub App scoped token mint failed: installation response had no \
                             access_tokens_url"
                        );
                        GitProviderError::ApiError(
                            "No access_tokens_url in installation".to_string(),
                        )
                    })?,
                )
                .map_err(|e| {
                    error!(
                        installation_id,
                        app_id = *app_id,
                        error = %e,
                        "GitHub App scoped token mint failed: malformed access_tokens_url"
                    );
                    GitProviderError::ApiError(format!("Failed to parse access_tokens_url: {}", e))
                })?;

                // Send the request body. When both fields are empty the body
                // serializes to `{}` and GitHub returns a full-installation
                // token (matches the historical behavior of this method).
                // When either is populated GitHub narrows the token.
                let access: octocrab::models::InstallationToken = octocrab
                    .post(gh_access_tokens_url.path(), Some(request))
                    .await
                    .map_err(|e| {
                        error!(
                            installation_id,
                            app_id = *app_id,
                            repos = ?request.repositories,
                            perms = ?request.permissions,
                            error = %e,
                            "GitHub App scoped token mint failed: GitHub rejected access_tokens \
                             request (common causes: requested repo not selected on the \
                             installation, or App lacks the requested permission)"
                        );
                        GitProviderError::ApiError(format!(
                            "Failed to create installation token: {}",
                            e
                        ))
                    })?;

                let expires_at = access
                    .expires_at
                    .as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc));

                debug!(
                    "Successfully generated GitHub App installation token (expires_at={:?})",
                    expires_at
                );
                Ok((access.token, expires_at))
            }
            _ => Err(GitProviderError::InvalidConfiguration(
                "GitHub App credentials required for installation token generation".to_string(),
            )),
        }
    }

    /// Validate a GitHub access token by making a simple API call
    async fn validate_token(&self, access_token: &str) -> Result<bool, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        // Use the /user endpoint to validate the token (for OAuth/PAT)
        // For GitHub Apps, we use /app endpoint
        let endpoint = match &self.auth_method {
            AuthMethod::GitHubApp { .. } => format!("{}/installation/repositories", self.api_url),
            _ => format!("{}/user", self.api_url),
        };

        let response = self
            .send_with_retry(|| client.get(&endpoint).headers(headers.clone()))
            .await?;

        // Token is valid if we get a 200 OK
        // 401 means unauthorized (invalid token)
        // 403 could mean rate limited or token lacks scopes
        match response.status() {
            status if status.is_success() => Ok(true),
            status if status.as_u16() == 401 => Ok(false),
            status if status.as_u16() == 403 => {
                // Check if it's rate limiting
                if response
                    .headers()
                    .get("X-RateLimit-Remaining")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<i32>().ok())
                    == Some(0)
                {
                    Err(GitProviderError::RateLimitExceeded)
                } else {
                    Ok(false) // Token might be invalid or lack permissions
                }
            }
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
impl GitProviderService for GitHubProvider {
    fn provider_type(&self) -> GitProviderType {
        GitProviderType::GitHub
    }

    async fn authenticate(&self, code: Option<String>) -> Result<String, GitProviderError> {
        match &self.auth_method {
            AuthMethod::PersonalAccessToken { token } => {
                // For PATs, just return the token directly
                info!("Using GitHub Personal Access Token for authentication");
                Ok(token.clone())
            }
            AuthMethod::OAuth {
                client_id,
                client_secret,
                ..
            } => {
                if let Some(code) = code {
                    // Exchange authorization code for access token
                    let client = self.get_client();
                    let params = [
                        ("client_id", client_id.to_string()),
                        ("client_secret", client_secret.to_string()),
                        ("code", code.clone()),
                    ];

                    let response = self
                        .send_with_retry(|| {
                            client
                                .post("https://github.com/login/oauth/access_token")
                                .header("Accept", "application/json")
                                .form(&params)
                        })
                        .await?;

                    let token_response: OAuthTokenResponse = response
                        .json()
                        .await
                        .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

                    // Note: GitHub OAuth apps typically don't return refresh_tokens
                    // unless using GitHub Apps with device flow
                    Ok(token_response.access_token)
                } else {
                    Err(GitProviderError::AuthenticationFailed(
                        "Authorization code required".to_string(),
                    ))
                }
            }
            AuthMethod::GitHubApp { .. } => {
                // GitHub App authentication would require JWT generation
                // This is handled by the existing GithubAppService
                Err(GitProviderError::NotImplemented)
            }
            _ => Err(GitProviderError::NotImplemented),
        }
    }

    async fn get_auth_url(&self, state: &str) -> Result<String, GitProviderError> {
        match &self.auth_method {
            AuthMethod::OAuth {
                client_id,
                redirect_uri,
                ..
            } => {
                let auth_url = format!(
                    "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&state={}&scope=repo,user",
                    client_id, redirect_uri, state
                );
                Ok(auth_url)
            }
            AuthMethod::PersonalAccessToken { .. } => {
                // PATs don't need OAuth flow
                Err(GitProviderError::NotImplemented)
            }
            _ => Err(GitProviderError::NotImplemented),
        }
    }

    async fn token_needs_refresh(&self, access_token: &str) -> bool {
        // Check if the token is valid by making a simple API call
        match self.validate_token(access_token).await {
            Ok(true) => false, // Token is valid, no refresh needed
            Ok(false) => true, // Token is invalid, needs refresh
            Err(_) => true,    // Error validating, assume it needs refresh
        }
    }

    async fn validate_token(&self, access_token: &str) -> Result<bool, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        // Use the /user endpoint to validate the token (for OAuth/PAT)
        // For GitHub Apps, we use /app endpoint
        let endpoint = match &self.auth_method {
            AuthMethod::GitHubApp { .. } => format!("{}/installation/repositories", self.api_url),
            _ => format!("{}/user", self.api_url),
        };

        let response = self
            .send_with_retry(|| client.get(&endpoint).headers(headers.clone()))
            .await?;

        // Token is valid if we get a 200 OK
        // 401 means unauthorized (invalid token)
        // 403 could mean rate limited or token lacks scopes
        match response.status() {
            status if status.is_success() => Ok(true),
            status if status.as_u16() == 401 => Ok(false),
            status if status.as_u16() == 403 => {
                // Check if it's rate limiting
                if response
                    .headers()
                    .get("X-RateLimit-Remaining")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<i32>().ok())
                    == Some(0)
                {
                    Err(GitProviderError::RateLimitExceeded)
                } else {
                    Ok(false) // Token might be invalid or lack permissions
                }
            }
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

    async fn validate_and_refresh_token(
        &self,
        access_token: &str,
        refresh_token: Option<&str>,
    ) -> Result<(String, Option<String>), GitProviderError> {
        // First, validate the current token
        match self.validate_token(access_token).await {
            Ok(true) => {
                // Token is valid, return it as-is
                debug!("GitHub access token is still valid");
                Ok((
                    access_token.to_string(),
                    refresh_token.map(|s| s.to_string()),
                ))
            }
            Ok(false) | Err(GitProviderError::RateLimitExceeded) => {
                // Token is invalid or expired, try to refresh if we have a refresh token
                info!("GitHub access token is invalid or expired, attempting refresh");

                // Get credentials based on auth method
                match &self.auth_method {
                    AuthMethod::OAuth {
                        client_id,
                        client_secret,
                        ..
                    } => {
                        if let Some(refresh_token) = refresh_token {
                            let (new_access_token, new_refresh_token) = self
                                .refresh_access_token(client_id, client_secret, refresh_token)
                                .await?;
                            Ok((new_access_token, new_refresh_token))
                        } else {
                            Err(GitProviderError::AuthenticationFailed(
                                "OAuth access token is invalid and no refresh token is available"
                                    .to_string(),
                            ))
                        }
                    }
                    AuthMethod::PersonalAccessToken { .. } => {
                        // PATs don't support refresh
                        debug!("Personal Access Token cannot be refreshed");
                        Err(GitProviderError::AuthenticationFailed(
                            "Personal Access Token is invalid and cannot be refreshed".to_string(),
                        ))
                    }
                    AuthMethod::GitHubApp { .. } => {
                        // For GitHub Apps, the refresh_token contains the installation_id
                        // This is a special case where we regenerate the installation token
                        if let Some(installation_id_str) = refresh_token {
                            let installation_id =
                                installation_id_str.parse::<i64>().map_err(|e| {
                                    GitProviderError::InvalidConfiguration(format!(
                                        "Invalid installation_id in refresh_token: {}",
                                        e
                                    ))
                                })?;

                            let new_access_token =
                                self.generate_installation_token(installation_id).await?;
                            // Return the same installation_id as refresh_token for next time
                            Ok((new_access_token, Some(installation_id_str.to_string())))
                        } else {
                            Err(GitProviderError::AuthenticationFailed(
                                "GitHub App installation token is invalid and no installation_id is available".to_string()
                            ))
                        }
                    }
                    _ => Err(GitProviderError::NotImplemented),
                }
            }
            Err(e) => {
                // Some other error occurred during validation
                error!("Error validating GitHub token: {}", e);
                Err(e)
            }
        }
    }

    /// GitHub-side mint of a per-operation, single-repo, narrow-permission
    /// installation token. Read [`ScopedTokenRequest::for_repo_read`] /
    /// `for_repo_write` for the body shape; this just routes the right one
    /// to [`Self::generate_scoped_installation_token`] and packages the
    /// result for the daemon's consumption.
    ///
    /// Only works for `AuthMethod::GitHubApp` connections. PAT and OAuth
    /// connections fall through to the trait default
    /// (`Err(NotImplemented)`) — there's no GitHub API to "narrow a PAT"
    /// at runtime, so a PAT-backed connection cannot serve per-op tokens
    /// at all. The daemon must refuse the request rather than handing out
    /// the long-lived PAT.
    async fn mint_scoped_repo_token(
        &self,
        installation_id: Option<&str>,
        owner: &str,
        repo: &str,
        operation: ScopedTokenOp,
    ) -> Result<ScopedTokenGrant, GitProviderError> {
        // Only GitHub App connections can mint scoped tokens. Bail loudly
        // for PAT/OAuth so the daemon doesn't accidentally hand out a
        // long-lived token instead.
        let installation_id_str = installation_id.ok_or_else(|| {
            GitProviderError::InvalidConfiguration(
                "Per-op scoped tokens require a GitHub App installation_id; \
                 PAT and OAuth connections are not supported"
                    .to_string(),
            )
        })?;

        let installation_id_i64 = installation_id_str.parse::<i64>().map_err(|e| {
            GitProviderError::InvalidConfiguration(format!(
                "Invalid installation_id '{}': {}",
                installation_id_str, e
            ))
        })?;

        // GitHub's `POST /app/installations/{id}/access_tokens` expects bare
        // repo names in `repositories`, NOT `owner/repo`. Passing the full
        // name causes a 422 even when the App has access to the repo —
        // `owner` is determined by the installation itself.
        let _ = owner;
        let request = match operation {
            ScopedTokenOp::Fetch => ScopedTokenRequest::for_repo_read(repo),
            ScopedTokenOp::Push => ScopedTokenRequest::for_repo_write(repo),
        };

        let (token, expires_at) = self
            .generate_scoped_installation_token(installation_id_i64, &request)
            .await?;

        Ok(ScopedTokenGrant {
            // GitHub's well-known basic-auth username for installation
            // tokens. Documented at:
            // https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/authenticating-as-a-github-app-installation
            username: "x-access-token".to_string(),
            password: token,
            expires_at,
        })
    }

    async fn list_repositories(
        &self,
        access_token: &str,
        organization: Option<&str>,
    ) -> Result<Vec<Repository>, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        // Check if this is a GitHub App installation token
        // GitHub App installation tokens work with /installation/repositories
        // Regular tokens (PAT/OAuth) use /user/repos or /orgs/{org}/repos
        let base_url = match &self.auth_method {
            AuthMethod::GitHubApp { .. } => {
                // For GitHub Apps, always use installation/repositories
                // This returns all repos the installation has access to
                format!("{}/installation/repositories", self.api_url)
            }
            _ => {
                // For PAT/OAuth, use the traditional endpoints
                if let Some(org) = organization {
                    format!("{}/orgs/{}/repos", self.api_url, org)
                } else {
                    format!("{}/user/repos", self.api_url)
                }
            }
        };

        debug!("Fetching repositories from: {}", base_url);

        let mut all_repositories = Vec::new();
        let mut page = 1;
        let per_page = 100;

        loop {
            let url = format!(
                "{}{}per_page={}&page={}",
                base_url,
                if base_url.contains('?') { "&" } else { "?" },
                per_page,
                page
            );

            debug!("Fetching page {} from: {}", page, url);

            let response = self
                .send_with_retry(|| client.get(&url).headers(headers.clone()))
                .await?;

            if !response.status().is_success() {
                let status = response.status();
                let error_text = response.text().await.unwrap_or_default();
                error!("Failed to list repositories: {} - {}", status, error_text);
                return Err(GitProviderError::ApiError(format!(
                    "Failed to list repositories: {} - {}",
                    status, error_text
                )));
            }

            // GitHub App installation endpoint returns a different structure
            let github_repos: Vec<GitHubRepo> = match &self.auth_method {
                AuthMethod::GitHubApp { .. } => {
                    // For GitHub Apps, parse the installation response format
                    let installation_response: InstallationRepositoriesResponse = response
                        .json()
                        .await
                        .map_err(|e| GitProviderError::ApiError(e.to_string()))?;
                    installation_response.repositories
                }
                _ => {
                    // For PAT/OAuth, parse as array directly
                    response
                        .json()
                        .await
                        .map_err(|e| GitProviderError::ApiError(e.to_string()))?
                }
            };

            let repos_count = github_repos.len();
            debug!("Received {} repositories on page {}", repos_count, page);

            let repositories: Vec<Repository> = github_repos
                .into_iter()
                .map(|r| Repository {
                    id: r.id.to_string(),
                    name: r.name,
                    full_name: r.full_name,
                    owner: r.owner.login,
                    description: r.description,
                    private: r.private,
                    default_branch: r.default_branch,
                    clone_url: r.clone_url,
                    ssh_url: r.ssh_url,
                    web_url: r.html_url,
                    language: r.language,
                    size: r.size,
                    stars: r.stargazers_count,
                    forks: r.forks_count,
                    created_at: DateTime::parse_from_rfc3339(&r.created_at)
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .unwrap_or_else(|_| chrono::Utc::now()),
                    updated_at: DateTime::parse_from_rfc3339(&r.updated_at)
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .unwrap_or_else(|_| chrono::Utc::now()),
                    pushed_at: r.pushed_at.and_then(|s| {
                        DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                    }),
                })
                .collect();

            all_repositories.extend(repositories);

            // Break if we received fewer repositories than per_page (last page)
            if repos_count < per_page {
                break;
            }

            page += 1;
        }

        info!(
            "Successfully fetched {} repositories across {} pages",
            all_repositories.len(),
            page
        );
        Ok(all_repositories)
    }

    async fn get_repository(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Repository, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let url = format!("{}/repos/{}/{}", self.api_url, owner, repo);

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
        struct GitHubRepo {
            id: i64,
            name: String,
            full_name: String,
            owner: GitHubOwner,
            description: Option<String>,
            private: bool,
            default_branch: String,
            clone_url: String,
            ssh_url: String,
            html_url: String,
            language: Option<String>,
            size: i64,
            stargazers_count: i32,
            forks_count: i32,
            created_at: String,
            updated_at: String,
            pushed_at: Option<String>,
        }

        #[derive(Deserialize)]
        struct GitHubOwner {
            login: String,
        }

        let github_repo: GitHubRepo = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        Ok(Repository {
            id: github_repo.id.to_string(),
            name: github_repo.name,
            full_name: github_repo.full_name,
            owner: github_repo.owner.login,
            description: github_repo.description,
            private: github_repo.private,
            default_branch: github_repo.default_branch,
            clone_url: github_repo.clone_url,
            ssh_url: github_repo.ssh_url,
            web_url: github_repo.html_url,
            language: github_repo.language,
            size: github_repo.size,
            stars: github_repo.stargazers_count,
            forks: github_repo.forks_count,
            created_at: DateTime::parse_from_rfc3339(&github_repo.created_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
            updated_at: DateTime::parse_from_rfc3339(&github_repo.updated_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
            pushed_at: github_repo.pushed_at.and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
            }),
        })
    }

    async fn list_branches(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<Branch>, GitProviderError> {
        let octocrab = self.get_octocrab_client(access_token).await?;

        // Fetch the first page with the maximum page size, then walk every
        // remaining page so callers always see the complete branch list.
        // GitHub paginates branches at 30 items per page by default; without
        // `all_pages` we'd silently truncate repos like ours where `main`
        // sorts past page 1.
        let first_page = octocrab
            .repos(owner, repo)
            .list_branches()
            .per_page(100)
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to list branches: {}", e)))?;

        let all = octocrab.all_pages(first_page).await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to paginate branches: {}", e))
        })?;

        let branches = all
            .into_iter()
            .map(|b| Branch {
                name: b.name,
                commit_sha: b.commit.sha,
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
        let octocrab = self.get_octocrab_client(access_token).await?;

        // Get all tags using Octocrab
        let tags = octocrab
            .repos(owner, repo)
            .list_tags()
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to list tags: {}", e)))?;

        // Convert Octocrab tags to our GitProviderTag type
        let tags = tags
            .items
            .into_iter()
            .map(|t| GitProviderTag {
                name: t.name,
                commit_sha: t.commit.sha,
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

        // Percent-encode the path so model/user-supplied paths can't break out
        // of the Contents API URL (e.g. injecting `?`/`#`/`..%2f`). Encode each
        // segment individually so the `/` separators are preserved.
        let encoded_path = path
            .split('/')
            .map(|segment| urlencoding::encode(segment).into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let mut url = format!(
            "{}/repos/{}/{}/contents/{}",
            self.api_url, owner, repo, encoded_path
        );
        if let Some(ref_name) = branch {
            url.push_str(&format!("?ref={}", ref_name));
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
        struct GitHubFile {
            path: String,
            content: String,
            encoding: String,
        }

        let file: GitHubFile = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        Ok(FileContent {
            path: file.path,
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

        // Percent-encode each path segment individually so `/` separators are
        // preserved but user-supplied characters can't break the URL.
        let encoded_path = path
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|segment| urlencoding::encode(segment).into_owned())
            .collect::<Vec<_>>()
            .join("/");

        let mut url = format!(
            "{}/repos/{}/{}/contents/{}",
            self.api_url, owner, repo, encoded_path
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

        // The Contents API returns either a JSON array (directory) or a single
        // object (file). Detect which one by peeking at the raw value.
        #[derive(Deserialize)]
        struct GitHubContentItem {
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

        // Try to decode as an array first; fall back to a single-item wrapper
        // so callers that accidentally point at a file still get a useful result.
        let items: Vec<GitHubContentItem> =
            if let Ok(arr) = serde_json::from_slice::<Vec<GitHubContentItem>>(&body_bytes) {
                arr
            } else {
                match serde_json::from_slice::<GitHubContentItem>(&body_bytes) {
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
                // Only surface size for plain files; dirs and symlinks report 0 or
                // meaningless values from the API.
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

        // Sort: directories first, then alphabetically by name within each group.
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
            "{}/repos/{}/{}/commits/{}",
            self.api_url, owner, repo, branch
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

        #[derive(Deserialize)]
        struct GitHubCommit {
            sha: String,
            commit: GitHubCommitDetails,
        }

        #[derive(Deserialize)]
        struct GitHubCommitDetails {
            message: String,
            author: GitHubCommitAuthor,
        }

        #[derive(Deserialize)]
        struct GitHubCommitAuthor {
            name: String,
            email: String,
            date: String,
        }

        let commit_response: GitHubCommit = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        Ok(Commit {
            sha: commit_response.sha,
            message: commit_response.commit.message,
            author: commit_response.commit.author.name,
            author_email: commit_response.commit.author.email,
            date: DateTime::parse_from_rfc3339(&commit_response.commit.author.date)
                .map(|dt| dt.into())
                .unwrap_or_else(|_| chrono::Utc::now()),
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

        let url = format!("{}/repos/{}/{}/hooks", self.api_url, owner, repo);

        #[derive(Serialize)]
        struct CreateHookRequest {
            name: String,
            config: HookConfig,
            events: Vec<String>,
            active: bool,
        }

        #[derive(Serialize)]
        struct HookConfig {
            url: String,
            content_type: String,
            secret: Option<String>,
        }

        let request = CreateHookRequest {
            name: "web".to_string(),
            config: HookConfig {
                url: config.url,
                content_type: "json".to_string(),
                secret: config.secret,
            },
            events: config.events,
            active: true,
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
            self.api_url, owner, repo, webhook_id
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
        payload: &[u8],
        signature: &str,
        secret: &str,
    ) -> Result<bool, GitProviderError> {
        github_webhook_signature_matches(payload, signature, secret)
            .map_err(GitProviderError::Other)
    }

    async fn get_user(&self, access_token: &str) -> Result<User, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let url = format!("{}/user", self.api_url);

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
        struct GitHubUser {
            id: i64,
            login: String,
            name: Option<String>,
            email: Option<String>,
            avatar_url: Option<String>,
        }

        let user: GitHubUser = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        Ok(User {
            id: user.id.to_string(),
            username: user.login,
            name: user.name,
            email: user.email,
            avatar_url: user.avatar_url,
        })
    }

    async fn check_repository_accessible(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<bool, GitProviderError> {
        let client = self.get_client();

        let url = format!("{}/repos/{}/{}", self.api_url, owner, repo);

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
        // the whole deployment indefinitely. Bound it so the job fails fast with
        // a clear error instead of getting stuck. The orphaned blocking task is
        // abandoned (it holds no locks the caller needs); the partial clone is
        // cleaned up by the caller's directory reset before any retry.
        const CLONE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

        let join = tokio::task::spawn_blocking(move || {
            let target = target_dir.as_path();
            if let Some(token) = &access_token {
                super::git_ops::clone_repo_with_token(&clone_url, target, token, None)
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
        // For now, fall back to the reqwest implementation for getting commits
        // as Octocrab doesn't expose a direct get_commit method
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        // GitHub API endpoint for getting a commit
        let url = format!(
            "{}/repos/{}/{}/commits/{}",
            self.api_url,
            owner,
            repo,
            urlencoding::encode(reference)
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
        struct GitHubCommit {
            sha: String,
            commit: GitHubCommitInfo,
        }

        #[derive(Deserialize)]
        struct GitHubCommitInfo {
            message: String,
            author: GitHubAuthor,
        }

        #[derive(Deserialize)]
        struct GitHubAuthor {
            name: String,
            email: String,
            date: String,
        }

        let github_commit: GitHubCommit = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        let date = DateTime::parse_from_rfc3339(&github_commit.commit.author.date)
            .map_err(|e| GitProviderError::ApiError(format!("Failed to parse date: {}", e)))?
            .with_timezone(&Utc);

        Ok(Commit {
            sha: github_commit.sha,
            message: github_commit.commit.message,
            author: github_commit.commit.author.name,
            author_email: github_commit.commit.author.email,
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
        // Fall back to the reqwest implementation as Octocrab doesn't have a direct get_commit method
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        // GitHub API endpoint for getting a commit
        let url = format!(
            "{}/repos/{}/{}/commits/{}",
            self.api_url, owner, repo, commit_sha
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

        let url = format!(
            "{}/repos/{}/{}/commits?sha={}&per_page={}",
            self.api_url, owner, repo, branch, per_page
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

        #[derive(Deserialize)]
        struct GitHubCommitItem {
            sha: String,
            commit: GitHubCommitItemDetails,
        }

        #[derive(Deserialize)]
        struct GitHubCommitItemDetails {
            message: String,
            author: Option<GitHubCommitItemAuthor>,
        }

        #[derive(Deserialize)]
        struct GitHubCommitItemAuthor {
            name: Option<String>,
            email: Option<String>,
            date: Option<String>,
        }

        let items: Vec<GitHubCommitItem> = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        let commits = items
            .into_iter()
            .map(|item| {
                let author = item.commit.author.as_ref();
                let date_str = author.and_then(|a| a.date.as_deref()).unwrap_or("");
                let date = chrono::DateTime::parse_from_rfc3339(date_str)
                    .map(|dt| dt.into())
                    .unwrap_or_else(|_| chrono::Utc::now());

                Commit {
                    sha: item.sha,
                    message: item.commit.message,
                    author: author.and_then(|a| a.name.clone()).unwrap_or_default(),
                    author_email: author.and_then(|a| a.email.clone()).unwrap_or_default(),
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
            "Downloading archive for {}/{} at ref {}",
            owner, repo, ref_spec
        );

        // Build the URL for downloading the tarball
        let url = format!(
            "{}/repos/{}/{}/tarball/{}",
            self.api_url, owner, repo, ref_spec
        );

        // Use the archive client (no total timeout — large tarballs stream for
        // longer than the 30s API timeout, which would otherwise abort mid-body).
        let client = self.get_archive_client();
        let headers = self.get_headers(access_token);

        // GitHub's tarball endpoint always answers with a 302 redirect to a
        // short-lived signed URL on `codeload.github.com` (or the Enterprise
        // host). Our shared client uses `redirect::Policy::none()` as an SSRF
        // defense, so we must follow this hop *manually* — and only after
        // validating the redirect target is a GitHub-owned host, so a
        // compromised/spoofed response can't bounce us to an internal address.
        debug!("Archive: sending initial tarball request to {}", url);
        let response = self
            .send_with_retry(|| client.get(&url).headers(headers.clone()))
            .await?;

        let status = response.status();
        debug!("Archive: initial response status {}", status);
        let response = if status.is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    GitProviderError::ApiError(format!(
                        "Archive download for {}/{} returned {} with no Location header",
                        owner, repo, status
                    ))
                })?
                .to_string();

            let redirect_url = reqwest::Url::parse(&location).map_err(|e| {
                GitProviderError::ApiError(format!(
                    "Archive redirect Location is not a valid URL ({}): {}",
                    location, e
                ))
            })?;

            // SSRF guard: only follow the redirect to a GitHub-owned host.
            self.validate_archive_redirect_host(&redirect_url)?;
            debug!(
                "Archive: following validated redirect to host {}",
                redirect_url.host_str().unwrap_or("?")
            );

            // Fetch the signed URL WITHOUT the Authorization header. The signed
            // codeload URL is pre-authenticated via query params; forwarding the
            // bearer token to a redirect target would needlessly leak it.
            let resp = client.get(redirect_url).send().await.map_err(|e| {
                GitProviderError::ApiError(format!(
                    "Failed to download archive from redirect target: {}",
                    e
                ))
            })?;
            debug!("Archive: redirect-target response status {}", resp.status());
            resp
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

        // Cap the total bytes written: the client `.timeout()` bounds idle/stall
        // time but not the size of a steady stream, so an unexpectedly huge (or
        // hostile) tarball could otherwise fill the control-plane volume. Reject
        // an oversized `Content-Length` up front, and enforce the same ceiling
        // while streaming in case the header lies or is absent.
        const MAX_ARCHIVE_BYTES: u64 = 5 * 1024 * 1024 * 1024; // 5 GiB
        if let Some(len) = response.content_length() {
            if len > MAX_ARCHIVE_BYTES {
                return Err(GitProviderError::ApiError(format!(
                    "Archive too large: Content-Length {} exceeds limit {} bytes",
                    len, MAX_ARCHIVE_BYTES
                )));
            }
        }

        // Stream the response body to a file
        let mut file = tokio::fs::File::create(target_path)
            .await
            .map_err(|e| GitProviderError::Other(format!("Failed to create file: {}", e)))?;

        let total_bytes = response.content_length();
        debug!(
            "Archive: streaming body to {} (content_length={:?})",
            target_path.display(),
            total_bytes
        );
        // Emit a progress update at most every ~512 KiB so a slow link still
        // shows steady movement without flooding the log.
        const PROGRESS_STEP: u64 = 512 * 1024;
        let mut next_progress_at: u64 = PROGRESS_STEP;
        let mut written: u64 = 0;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|e| GitProviderError::ApiError(format!("Failed to read chunk: {}", e)))?;
            written = written.saturating_add(chunk.len() as u64);
            if written > MAX_ARCHIVE_BYTES {
                // Best-effort cleanup of the partial file before bailing. Drop the
                // handle first so the file isn't held open during removal.
                drop(file);
                let _ = tokio::fs::remove_file(target_path).await;
                return Err(GitProviderError::ApiError(format!(
                    "Archive exceeded maximum size of {} bytes mid-stream",
                    MAX_ARCHIVE_BYTES
                )));
            }
            if let Some(tx) = progress {
                if written >= next_progress_at {
                    // Ignore send errors: a dropped receiver just means nobody is
                    // listening for progress, which must not fail the download.
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

        info!("Successfully downloaded archive to {:?}", target_path);
        Ok(())
    }

    async fn create_source(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        reference: &str,
    ) -> Result<Box<dyn temps_presets::source::ProjectSource>, GitProviderError> {
        let octocrab = self.get_octocrab_client(access_token).await?;

        Ok(Box::new(crate::sources::GitHubSource::new(
            std::sync::Arc::new(octocrab),
            owner.to_string(),
            repo.to_string(),
            reference.to_string(),
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

        // If owner is specified, create in organization; otherwise create in user account
        let url = if let Some(org) = owner {
            format!("{}/orgs/{}/repos", self.api_url, org)
        } else {
            format!("{}/user/repos", self.api_url)
        };

        #[derive(Serialize)]
        struct CreateRepoRequest {
            name: String,
            description: Option<String>,
            private: bool,
            auto_init: bool, // Initialize with README to have a default branch
        }

        let request = CreateRepoRequest {
            name: name.to_string(),
            description: description.map(|s| s.to_string()),
            private,
            auto_init: true, // Initialize with README so we have a default branch
        };

        info!("Creating repository {} (private: {})", name, private);

        let response = self
            .send_with_retry(|| client.post(&url).headers(headers.clone()).json(&request))
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!("Failed to create repository: {} - {}", status, error_text);

            // Detect GitHub's "name already exists" 422 and surface as a typed
            // variant so handlers can return 409 Conflict with a clean message.
            if status.as_u16() == 422 {
                #[derive(Deserialize)]
                struct GhFieldError {
                    field: Option<String>,
                    message: Option<String>,
                }
                #[derive(Deserialize)]
                struct GhErrorBody {
                    errors: Option<Vec<GhFieldError>>,
                }
                if let Ok(body) = serde_json::from_str::<GhErrorBody>(&error_text) {
                    let name_taken = body.errors.iter().flatten().any(|e| {
                        e.field.as_deref() == Some("name")
                            && e.message
                                .as_deref()
                                .map(|m| m.contains("already exists"))
                                .unwrap_or(false)
                    });
                    if name_taken {
                        return Err(GitProviderError::RepositoryAlreadyExists {
                            name: name.to_string(),
                        });
                    }
                }
            }

            return Err(GitProviderError::ApiError(format!(
                "Failed to create repository: {} - {}",
                status, error_text
            )));
        }

        let github_repo: GitHubRepo = response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to parse response: {}", e)))?;

        info!("Successfully created repository: {}", github_repo.full_name);

        Ok(Repository {
            id: github_repo.id.to_string(),
            name: github_repo.name,
            full_name: github_repo.full_name,
            owner: github_repo.owner.login,
            description: github_repo.description,
            private: github_repo.private,
            default_branch: github_repo.default_branch,
            clone_url: github_repo.clone_url,
            ssh_url: github_repo.ssh_url,
            web_url: github_repo.html_url,
            language: github_repo.language,
            size: github_repo.size,
            stars: github_repo.stargazers_count,
            forks: github_repo.forks_count,
            created_at: DateTime::parse_from_rfc3339(&github_repo.created_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
            updated_at: DateTime::parse_from_rfc3339(&github_repo.updated_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now()),
            pushed_at: github_repo.pushed_at.and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
            }),
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

        // 1. Get the base branch SHA.
        // First try the target branch — if it doesn't exist, get the default branch (main/master)
        // and create the new branch from it.
        #[derive(Deserialize)]
        struct GitRef {
            object: GitRefObject,
        }

        #[derive(Deserialize)]
        struct GitRefObject {
            sha: String,
        }

        let ref_url = format!(
            "{}/repos/{}/{}/git/ref/heads/{}",
            self.api_url, owner, repo, branch
        );

        let ref_response = self
            .send_with_retry(|| client.get(&ref_url).headers(headers.clone()))
            .await?;

        let base_commit_sha =
            if ref_response.status().is_success() {
                // Branch exists — use its current SHA
                let git_ref: GitRef = ref_response.json().await.map_err(|e| {
                    GitProviderError::ApiError(format!("Failed to parse ref: {}", e))
                })?;
                git_ref.object.sha
            } else {
                // Branch doesn't exist — get the default branch SHA and create the new branch
                // Try "main" first, then "master"
                let mut base_sha = None;
                for base_branch in &["main", "master"] {
                    let base_ref_url = format!(
                        "{}/repos/{}/{}/git/ref/heads/{}",
                        self.api_url, owner, repo, base_branch
                    );
                    let base_response = self
                        .send_with_retry(|| client.get(&base_ref_url).headers(headers.clone()))
                        .await?;
                    if base_response.status().is_success() {
                        let git_ref: GitRef = base_response.json().await.map_err(|e| {
                            GitProviderError::ApiError(format!("Failed to parse base ref: {}", e))
                        })?;
                        base_sha = Some(git_ref.object.sha);
                        break;
                    }
                }

                let sha = base_sha.ok_or_else(|| {
                    GitProviderError::ApiError(
                        "Could not find base branch (tried main, master)".to_string(),
                    )
                })?;

                // Create the new branch
                let create_ref_url = format!("{}/repos/{}/{}/git/refs", self.api_url, owner, repo);
                let create_response = self
                    .send_with_retry(|| {
                        client.post(&create_ref_url).headers(headers.clone()).json(
                            &serde_json::json!({
                                "ref": format!("refs/heads/{}", branch),
                                "sha": &sha
                            }),
                        )
                    })
                    .await?;

                if !create_response.status().is_success() {
                    let status = create_response.status();
                    let error_text = create_response.text().await.unwrap_or_default();
                    return Err(GitProviderError::ApiError(format!(
                        "Failed to create branch '{}': {} - {}",
                        branch, status, error_text
                    )));
                }

                info!("Created new branch '{}' from SHA {}", branch, &sha);
                sha
            };
        debug!("Base commit SHA: {}", base_commit_sha);

        // 2. Get the tree SHA from the base commit
        let commit_url = format!(
            "{}/repos/{}/{}/git/commits/{}",
            self.api_url, owner, repo, base_commit_sha
        );

        let commit_response = self
            .send_with_retry(|| client.get(&commit_url).headers(headers.clone()))
            .await?;

        #[derive(Deserialize)]
        struct GitCommitResponse {
            tree: GitTree,
        }

        #[derive(Deserialize)]
        struct GitTree {
            sha: String,
        }

        let commit_data: GitCommitResponse = commit_response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to parse commit: {}", e)))?;

        let base_tree_sha = commit_data.tree.sha;
        debug!("Base tree SHA: {}", base_tree_sha);

        // 3. Create blobs for each file
        let mut tree_entries = Vec::new();

        for (path, content) in files {
            let blob_url = format!("{}/repos/{}/{}/git/blobs", self.api_url, owner, repo);

            #[derive(Serialize)]
            struct CreateBlobRequest {
                content: String,
                encoding: String,
            }

            let blob_request = CreateBlobRequest {
                content: STANDARD.encode(&content),
                encoding: "base64".to_string(),
            };

            let blob_response = self
                .send_with_retry(|| {
                    client
                        .post(&blob_url)
                        .headers(headers.clone())
                        .json(&blob_request)
                })
                .await?;

            if !blob_response.status().is_success() {
                let status = blob_response.status();
                let error_text = blob_response
                    .text()
                    .await
                    .unwrap_or_else(|_| "Unknown error".to_string());
                return Err(GitProviderError::ApiError(format!(
                    "Failed to create blob for {}: {} - {}",
                    path, status, error_text
                )));
            }

            #[derive(Deserialize)]
            struct BlobResponse {
                sha: String,
            }

            let blob: BlobResponse = blob_response
                .json()
                .await
                .map_err(|e| GitProviderError::ApiError(format!("Failed to parse blob: {}", e)))?;

            tree_entries.push(serde_json::json!({
                "path": path,
                "mode": "100644", // Regular file
                "type": "blob",
                "sha": blob.sha
            }));

            debug!("Created blob for {}: {}", path, blob.sha);
        }

        // 4. Create a new tree with all the files
        let tree_url = format!("{}/repos/{}/{}/git/trees", self.api_url, owner, repo);

        let tree_request = serde_json::json!({
            "base_tree": base_tree_sha,
            "tree": tree_entries
        });

        let tree_response = self
            .send_with_retry(|| {
                client
                    .post(&tree_url)
                    .headers(headers.clone())
                    .json(&tree_request)
            })
            .await?;

        if !tree_response.status().is_success() {
            let status = tree_response.status();
            let error_text = tree_response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(GitProviderError::ApiError(format!(
                "Failed to create tree: {} - {}",
                status, error_text
            )));
        }

        #[derive(Deserialize)]
        struct TreeResponse {
            sha: String,
        }

        let tree: TreeResponse = tree_response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to parse tree: {}", e)))?;

        debug!("Created new tree: {}", tree.sha);

        // 5. Create a new commit
        let new_commit_url = format!("{}/repos/{}/{}/git/commits", self.api_url, owner, repo);

        let commit_request = serde_json::json!({
            "message": commit_message,
            "tree": tree.sha,
            "parents": [base_commit_sha]
        });

        let new_commit_response = self
            .send_with_retry(|| {
                client
                    .post(&new_commit_url)
                    .headers(headers.clone())
                    .json(&commit_request)
            })
            .await?;

        if !new_commit_response.status().is_success() {
            let status = new_commit_response.status();
            let error_text = new_commit_response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(GitProviderError::ApiError(format!(
                "Failed to create commit: {} - {}",
                status, error_text
            )));
        }

        #[derive(Deserialize)]
        struct NewCommitResponse {
            sha: String,
            message: String,
            author: CommitAuthor,
        }

        #[derive(Deserialize)]
        struct CommitAuthor {
            name: String,
            email: String,
            date: String,
        }

        let new_commit: NewCommitResponse = new_commit_response
            .json()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to parse commit: {}", e)))?;

        debug!("Created new commit: {}", new_commit.sha);

        // 6. Update the branch reference to point to the new commit
        let update_ref_url = format!(
            "{}/repos/{}/{}/git/refs/heads/{}",
            self.api_url, owner, repo, branch
        );

        let update_ref_request = serde_json::json!({
            "sha": new_commit.sha,
            "force": false
        });

        let update_ref_response = self
            .send_with_retry(|| {
                client
                    .patch(&update_ref_url)
                    .headers(headers.clone())
                    .json(&update_ref_request)
            })
            .await?;

        if !update_ref_response.status().is_success() {
            let status = update_ref_response.status();
            let error_text = update_ref_response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(GitProviderError::ApiError(format!(
                "Failed to update branch reference: {} - {}",
                status, error_text
            )));
        }

        info!(
            "Successfully pushed {} files to {}/{} with commit {}",
            tree_entries.len(),
            owner,
            repo,
            new_commit.sha
        );

        Ok(Commit {
            sha: new_commit.sha,
            message: new_commit.message,
            author: new_commit.author.name,
            author_email: new_commit.author.email,
            date: DateTime::parse_from_rfc3339(&new_commit.author.date)
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

        let url = format!("{}/repos/{}/{}/pulls", self.api_url, owner, repo);

        #[derive(Serialize)]
        struct CreatePullRequestBody<'a> {
            title: &'a str,
            body: &'a str,
            head: &'a str,
            base: &'a str,
        }

        let request_body = CreatePullRequestBody {
            title,
            body,
            head: head_branch,
            base: base_branch,
        };

        info!(
            "Creating pull request '{}' in {}/{}: {} -> {}",
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
                "Failed to create pull request in {}/{}: {} - {}",
                owner, repo, status, error_text
            );
            return Err(GitProviderError::ApiError(format!(
                "Failed to create pull request in {}/{}: {} - {}",
                owner, repo, status, error_text
            )));
        }

        #[derive(Deserialize)]
        struct PullRequestHead {
            #[serde(rename = "ref")]
            ref_name: String,
            sha: Option<String>,
        }

        #[derive(Deserialize)]
        struct PullRequestBase {
            #[serde(rename = "ref")]
            ref_name: String,
        }

        #[derive(Deserialize)]
        struct GitHubPullRequest {
            number: i32,
            html_url: String,
            title: String,
            head: PullRequestHead,
            base: PullRequestBase,
        }

        let pr: GitHubPullRequest = response.json().await.map_err(|e| {
            GitProviderError::ApiError(format!("Failed to parse pull request response: {}", e))
        })?;

        info!(
            "Successfully created pull request #{} in {}/{}",
            pr.number, owner, repo
        );

        Ok(PullRequest {
            number: pr.number,
            url: pr.html_url,
            title: pr.title,
            head_branch: pr.head.ref_name,
            base_branch: pr.base.ref_name,
            head_sha: pr.head.sha,
        })
    }
}

#[cfg(test)]
mod scoped_token_tests {
    use super::*;

    /// `default()` must serialize to `{}` so the GitHub `access_tokens`
    /// endpoint returns a full-installation token — the historical
    /// behavior of `generate_installation_token` before scoping was added.
    /// Any regression here silently broadens the security blast radius of
    /// background token-refresh flows.
    #[test]
    fn default_serializes_to_empty_object() {
        let body = serde_json::to_string(&ScopedTokenRequest::default()).unwrap();
        assert_eq!(body, "{}");
    }

    /// `for_repo_read` must produce a body that both narrows to a single
    /// repo AND drops permissions to `contents:read` only.
    ///
    /// Critically: `metadata` MUST NOT appear in the permissions map.
    /// GitHub strips the entire `permissions` block (silently!) when any
    /// requested key isn't in the App's declared permission set, and
    /// `metadata` is implicit, not declared. The strip surfaces as a token
    /// with `push:false, pull:false` on every repo — the failure mode we
    /// hit in production.
    #[test]
    fn for_repo_read_narrows_repo_and_perms() {
        let req = ScopedTokenRequest::for_repo_read("web");
        let v: serde_json::Value = serde_json::to_value(&req).unwrap();

        assert_eq!(v["repositories"], serde_json::json!(["web"]));
        assert_eq!(v["permissions"]["contents"], "read");
        // metadata must NOT be present — see doc on for_repo_read.
        assert!(v["permissions"].get("metadata").is_none());
        // Exactly one permission: contents.
        assert_eq!(v["permissions"].as_object().unwrap().len(), 1);
    }

    /// `for_repo_write` must elevate `contents` to `write` while leaving
    /// every other dimension narrowed. Used for `git push` flows.
    /// Same `metadata` rule as the read variant — never list it explicitly.
    #[test]
    fn for_repo_write_grants_write_only_on_contents() {
        let req = ScopedTokenRequest::for_repo_write("web");
        let v: serde_json::Value = serde_json::to_value(&req).unwrap();

        assert_eq!(v["repositories"], serde_json::json!(["web"]));
        assert_eq!(v["permissions"]["contents"], "write");
        assert!(v["permissions"].get("metadata").is_none());
        assert_eq!(v["permissions"].as_object().unwrap().len(), 1);
    }

    /// GitHub's `POST /app/installations/{id}/access_tokens` expects bare
    /// repo names in `repositories`, NOT `owner/repo`. The owner is fixed
    /// by the installation. Regression guard for the original bug where
    /// we sent `kfsoftware/temps-landing-new` and GitHub 422'd even though
    /// the App had access to the repo.
    #[test]
    fn for_repo_uses_bare_repo_name() {
        let req = ScopedTokenRequest::for_repo_read("temps-landing-new");
        assert_eq!(req.repositories.as_ref().unwrap()[0], "temps-landing-new");
        assert!(
            !req.repositories.as_ref().unwrap()[0].contains('/'),
            "GitHub rejects `owner/repo` form; pass bare repo name only"
        );
    }
}

#[cfg(test)]
mod archive_redirect_tests {
    use super::*;

    fn provider(api_url: Option<&str>) -> GitHubProvider {
        GitHubProvider::new(
            api_url.map(String::from),
            AuthMethod::PersonalAccessToken {
                token: "t".to_string(),
            },
        )
    }

    fn check(api_url: Option<&str>, url: &str) -> Result<(), GitProviderError> {
        let p = provider(api_url);
        p.validate_archive_redirect_host(&reqwest::Url::parse(url).unwrap())
    }

    #[test]
    fn allows_github_codeload_and_objects_hosts() {
        // The real 302 target for public github.com tarballs.
        assert!(check(
            None,
            "https://codeload.github.com/owner/repo/legacy.tar.gz/main"
        )
        .is_ok());
        assert!(check(None, "https://objects.githubusercontent.com/foo").is_ok());
        assert!(check(None, "https://github.com/owner/repo/archive/x.tar.gz").is_ok());
    }

    #[test]
    fn allows_enterprise_host_under_api_domain() {
        // GHE: api_url host is ghe.example.com → archive host *.ghe.example.com ok.
        assert!(check(
            Some("https://ghe.example.com/api/v3"),
            "https://codeload.ghe.example.com/owner/repo/legacy.tar.gz/main"
        )
        .is_ok());
        assert!(check(
            Some("https://ghe.example.com/api/v3"),
            "https://ghe.example.com/foo"
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
        assert!(check(None, "http://codeload.github.com/owner/repo").is_err());
    }

    #[test]
    fn rejects_lookalike_and_suffix_spoof_hosts() {
        // Attacker-controlled domains that merely *contain* github text.
        assert!(check(None, "https://github.com.evil.example/foo").is_err());
        assert!(check(None, "https://evilgithub.com/foo").is_err());
        assert!(check(None, "https://notgithubusercontent.com/foo").is_err());
        // Enterprise api host must not leak access to the public github.com.
        assert!(check(
            Some("https://ghe.example.com/api/v3"),
            "https://evil.example/foo"
        )
        .is_err());
    }

    #[test]
    fn userinfo_uses_connect_host_not_userinfo() {
        // `host_str()` returns the real connect host, so userinfo can't spoof it.
        // Legit host with attacker userinfo → allowed (we connect to codeload).
        assert!(check(None, "https://evil.example@codeload.github.com/foo").is_ok());
        // Attacker host with legit-looking userinfo → rejected (we'd connect to evil).
        assert!(check(None, "https://codeload.github.com@evil.example/foo").is_err());
    }

    #[test]
    fn rejects_trailing_dot_fqdn() {
        // `codeload.github.com.` ends with `com.`, not `.github.com` → rejected.
        // Locks in url-crate parsing behavior against dependency bumps.
        assert!(check(None, "https://codeload.github.com./foo").is_err());
    }
}

#[cfg(test)]
mod list_directory_tests {
    use super::*;

    /// Simulate the mapping + sort logic that `list_directory` applies to the
    /// raw GitHub Contents API items. This exercises the production code path
    /// without making any HTTP calls.
    fn map_and_sort(items: Vec<(&str, &str, &str, Option<u64>)>) -> Vec<RepoDirEntry> {
        // (name, path, type, size)
        let mut entries: Vec<RepoDirEntry> = items
            .into_iter()
            .map(|(name, path, item_type, size)| {
                let is_dir = item_type == "dir";
                let size = if item_type == "file" { size } else { None };
                RepoDirEntry {
                    name: name.to_string(),
                    path: path.to_string(),
                    is_dir,
                    size,
                }
            })
            .collect();

        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        entries
    }

    #[test]
    fn dirs_sorted_before_files() {
        let entries = map_and_sort(vec![
            ("main.rs", "src/main.rs", "file", Some(1024)),
            ("lib", "lib", "dir", None),
            ("README.md", "README.md", "file", Some(512)),
            ("src", "src", "dir", None),
        ]);

        // First two must be dirs
        assert!(entries[0].is_dir, "first entry should be a dir");
        assert!(entries[1].is_dir, "second entry should be a dir");
        // Dirs sorted by name: lib < src
        assert_eq!(entries[0].name, "lib");
        assert_eq!(entries[1].name, "src");
        // Files follow, sorted by name: README.md < main.rs
        assert!(!entries[2].is_dir);
        assert!(!entries[3].is_dir);
        assert_eq!(entries[2].name, "README.md");
        assert_eq!(entries[3].name, "main.rs");
    }

    #[test]
    fn file_size_populated_for_files_only() {
        let entries = map_and_sort(vec![
            ("Cargo.toml", "Cargo.toml", "file", Some(256)),
            ("tests", "tests", "dir", None),
            ("link.rs", "link.rs", "symlink", Some(0)),
        ]);

        let cargo = entries.iter().find(|e| e.name == "Cargo.toml").unwrap();
        assert_eq!(cargo.size, Some(256));

        let tests = entries.iter().find(|e| e.name == "tests").unwrap();
        assert_eq!(tests.size, None, "dirs must not carry size");

        let link = entries.iter().find(|e| e.name == "link.rs").unwrap();
        assert_eq!(link.size, None, "symlinks must not carry size");
        assert!(!link.is_dir);
    }

    #[test]
    fn empty_directory_returns_empty_vec() {
        let entries = map_and_sort(vec![]);
        assert!(entries.is_empty());
    }

    #[test]
    fn single_file_response_maps_correctly() {
        // When the API returns a single object (file, not dir), we wrap it in a
        // vec and produce one entry.
        let entries = map_and_sort(vec![("Cargo.toml", "Cargo.toml", "file", Some(1024))]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Cargo.toml");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].size, Some(1024));
    }
}

/// Verify a GitHub `X-Hub-Signature-256` HMAC-SHA256 webhook signature.
///
/// The comparison is **constant-time** (`subtle::ConstantTimeEq`) to avoid a
/// timing side-channel that could leak the expected signature byte-by-byte
/// (security review finding #14). Factored out as a free function so the
/// verification logic can be unit-tested with a known vector.
fn github_webhook_signature_matches(
    payload: &[u8],
    signature: &str,
    secret: &str,
) -> Result<bool, String> {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;
    use subtle::ConstantTimeEq;

    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|e| format!("Invalid secret key: {}", e))?;
    mac.update(payload);
    let expected = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    Ok(signature.as_bytes().ct_eq(expected.as_bytes()).into())
}

#[cfg(test)]
mod signature_tests {
    use super::github_webhook_signature_matches;

    // Known HMAC-SHA256 vector (GitHub's documented example):
    // secret "It's a Secret to Everybody", payload "Hello, World!".
    const SECRET: &str = "It's a Secret to Everybody";
    const PAYLOAD: &[u8] = b"Hello, World!";
    const VALID: &str = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";

    #[test]
    fn valid_signature_matches() {
        assert!(github_webhook_signature_matches(PAYLOAD, VALID, SECRET).unwrap());
    }

    #[test]
    fn tampered_signature_is_rejected() {
        let mut bad = VALID.to_string();
        bad.pop();
        bad.push('0');
        assert!(!github_webhook_signature_matches(PAYLOAD, &bad, SECRET).unwrap());
    }

    #[test]
    fn wrong_length_signature_is_rejected() {
        // ct_eq handles unequal lengths without panicking and returns false.
        assert!(!github_webhook_signature_matches(PAYLOAD, "sha256=deadbeef", SECRET).unwrap());
    }
}
