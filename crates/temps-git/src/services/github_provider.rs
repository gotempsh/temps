use super::git_provider::{
    AuthMethod, Branch, Commit, FileContent, GitProviderError, GitProviderService, GitProviderTag,
    GitProviderType, Repository, User, WebhookConfig,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use octocrab::{Octocrab, OctocrabBuilder};
use reqwest;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

// Response structs for API calls

/// OAuth token response (from /login/oauth/access_token)
/// GitHub OAuth typically doesn't include refresh_token
#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    token_type: Option<String>,
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
            .build()
            .unwrap()
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
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ];

        let response = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&params)
            .send()
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
    async fn generate_installation_token(
        &self,
        installation_id: i64,
    ) -> Result<String, GitProviderError> {
        match &self.auth_method {
            AuthMethod::GitHubApp {
                app_id,
                private_key,
                ..
            } => {
                info!(
                    "Generating GitHub App installation token for installation {}",
                    installation_id
                );

                // Create JWT for GitHub App authentication
                let app_id_param = octocrab::models::AppId(*app_id as u64);
                let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key.as_bytes()).map_err(
                    |e| {
                        GitProviderError::InvalidConfiguration(format!(
                            "Invalid private key: {}",
                            e
                        ))
                    },
                )?;

                let jwt = octocrab::auth::create_jwt(app_id_param, &key).map_err(|e| {
                    GitProviderError::ApiError(format!("Failed to create JWT: {}", e))
                })?;

                // Create octocrab instance with JWT
                let octocrab = OctocrabBuilder::new()
                    .personal_token(jwt)
                    .build()
                    .map_err(|e| {
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
                        GitProviderError::ApiError(format!("Failed to get installation: {}", e))
                    })?;

                // Create installation access token
                let create_access_token =
                    octocrab::params::apps::CreateInstallationAccessToken::default();
                let gh_access_tokens_url = reqwest::Url::parse(
                    installation.access_tokens_url.as_ref().ok_or_else(|| {
                        GitProviderError::ApiError(
                            "No access_tokens_url in installation".to_string(),
                        )
                    })?,
                )
                .map_err(|e| {
                    GitProviderError::ApiError(format!("Failed to parse access_tokens_url: {}", e))
                })?;

                let access: octocrab::models::InstallationToken = octocrab
                    .post(gh_access_tokens_url.path(), Some(&create_access_token))
                    .await
                    .map_err(|e| {
                        GitProviderError::ApiError(format!(
                            "Failed to create installation token: {}",
                            e
                        ))
                    })?;

                debug!("Successfully generated GitHub App installation token");
                Ok(access.token)
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

        let response = client
            .get(&endpoint)
            .headers(headers)
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to validate token: {}", e)))?;

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
                        ("client_id", client_id.as_str()),
                        ("client_secret", client_secret.as_str()),
                        ("code", &code),
                    ];

                    let response = client
                        .post("https://github.com/login/oauth/access_token")
                        .header("Accept", "application/json")
                        .form(&params)
                        .send()
                        .await
                        .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

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

        let response = client
            .get(&endpoint)
            .headers(headers)
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to validate token: {}", e)))?;

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

            let response = client
                .get(&url)
                .headers(headers.clone())
                .send()
                .await
                .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

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

        let response = client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

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

        // Get all branches using Octocrab
        let branches = octocrab
            .repos(owner, repo)
            .list_branches()
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to list branches: {}", e)))?;

        // Convert Octocrab branches to our Branch type
        let branches = branches
            .items
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

        let mut url = format!(
            "{}/repos/{}/{}/contents/{}",
            self.api_url, owner, repo, path
        );
        if let Some(ref_name) = branch {
            url.push_str(&format!("?ref={}", ref_name));
        }

        let response = client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

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

        let response = client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

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

        let response = client
            .post(&url)
            .headers(headers)
            .json(&request)
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

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

        let response = client
            .delete(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

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
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        // GitHub uses HMAC-SHA256 for webhook signatures
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
            .map_err(|e| GitProviderError::Other(format!("Invalid secret key: {}", e)))?;

        mac.update(payload);

        let expected = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        Ok(signature == expected)
    }

    async fn get_user(&self, access_token: &str) -> Result<User, GitProviderError> {
        let client = self.get_client();
        let headers = self.get_headers(access_token);

        let url = format!("{}/user", self.api_url);

        let response = client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

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

        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

        Ok(response.status().is_success())
    }

    async fn clone_repository(
        &self,
        clone_url: &str,
        target_dir: &str,
        access_token: Option<&str>,
    ) -> Result<(), GitProviderError> {
        use std::process::Command;

        let mut cmd = Command::new("git");
        cmd.arg("clone");

        if let Some(token) = access_token {
            // For GitHub, insert token in URL for HTTPS clones
            let authenticated_url = if clone_url.starts_with("https://github.com/") {
                clone_url.replace("https://", &format!("https://{}@", token))
            } else {
                clone_url.to_string()
            };
            cmd.arg(authenticated_url);
        } else {
            cmd.arg(clone_url);
        }

        cmd.arg(target_dir);

        let output = cmd
            .output()
            .map_err(|e| GitProviderError::Other(format!("Failed to execute git clone: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GitProviderError::Other(format!(
                "Git clone failed: {}",
                stderr
            )));
        }

        Ok(())
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
            self.api_url, owner, repo, reference
        );

        let response = client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

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

        let response = client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(e.to_string()))?;

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

    async fn download_archive(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        ref_spec: &str,
        target_path: &std::path::Path,
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

        let client = self.get_client();
        let mut headers = self.get_headers(access_token);
        // For archive downloads, we need to accept the tarball media type
        headers.insert(
            "Accept",
            reqwest::header::HeaderValue::from_static("application/vnd.github.v3.raw"),
        );

        let response = client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| GitProviderError::ApiError(format!("Failed to request archive: {}", e)))?;

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

        info!("Successfully downloaded archive to {:?}", target_path);
        Ok(())
    }
}
