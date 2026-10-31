// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! OAuth flows: issue and consume state tokens, start the flow and handle the
//! provider callback.

use super::*;

// OAuth scope constants
const GITLAB_OAUTH_SCOPES: &str = "api read_api read_repository";

impl GitProviderManager {
    /// Persist an OAuth state token tied to the user and provider that started the flow.
    /// Used so the unauthenticated OAuth callback can recover the user identity.
    async fn persist_oauth_state(
        &self,
        state: &str,
        user_id: i32,
        provider_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        use temps_entities::oauth_states;

        let expires_at = chrono::Utc::now() + chrono::Duration::minutes(15);
        oauth_states::ActiveModel {
            state: sea_orm::ActiveValue::Set(state.to_string()),
            user_id: sea_orm::ActiveValue::Set(user_id),
            provider_id: sea_orm::ActiveValue::Set(provider_id),
            expires_at: sea_orm::ActiveValue::Set(expires_at),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;
        Ok(())
    }

    /// Look up and consume a previously issued OAuth state token.
    /// Returns the (user_id, provider_id) that started the flow. Deletes the row so
    /// states are single-use, and rejects expired tokens.
    pub async fn consume_oauth_state(
        &self,
        state: &str,
    ) -> Result<(i32, i32), GitProviderManagerError> {
        use temps_entities::oauth_states;

        let row = oauth_states::Entity::find()
            .filter(oauth_states::Column::State.eq(state))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                GitProviderManagerError::OAuthStateInvalid(format!(
                    "State token not found: {}",
                    state
                ))
            })?;

        let now = chrono::Utc::now();
        let user_id = row.user_id;
        let provider_id = row.provider_id;
        let expired = row.expires_at < now;

        // Single-use: always delete the row after lookup.
        oauth_states::Entity::delete_by_id(row.id)
            .exec(self.db.as_ref())
            .await?;

        if expired {
            return Err(GitProviderManagerError::OAuthStateInvalid(format!(
                "State token expired: {}",
                state
            )));
        }

        Ok((user_id, provider_id))
    }

    /// Start OAuth flow for a git provider
    pub async fn start_oauth_flow(
        &self,
        provider_id: i32,
        user_id: i32,
        host_override: Option<String>,
    ) -> Result<(String, String), GitProviderManagerError> {
        let provider = self.get_provider(provider_id).await?;

        // Decrypt auth config to get OAuth credentials
        let auth_config = self.decrypt_sensitive_data(&provider.auth_config).await?;

        match GitProviderType::try_from(provider.provider_type.as_str())? {
            GitProviderType::GitHub => {
                // Extract OAuth credentials from auth config
                let client_id = auth_config["client_id"].as_str().ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration("Missing client_id".to_string())
                })?;

                // Generate state token for CSRF protection
                let state = uuid::Uuid::new_v4().to_string();
                self.persist_oauth_state(&state, user_id, provider_id)
                    .await?;

                // Calculate redirect URI based on host
                let redirect_uri = if let Some(host) = host_override {
                    format!("{}/git-providers/{}/callback", host, provider_id)
                } else if let Some(base_url) = &provider.base_url {
                    format!("{}/git-providers/{}/callback", base_url, provider_id)
                } else {
                    return Err(GitProviderManagerError::InvalidConfiguration(
                        "No base URL configured for OAuth flow".to_string(),
                    ));
                };

                // Build GitHub OAuth authorization URL
                let auth_url = format!(
                    "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&scope=user:email%20repo&state={}",
                    client_id,
                    urlencoding::encode(&redirect_uri),
                    state
                );

                Ok((auth_url, state))
            }
            GitProviderType::GitLab => {
                // Extract OAuth credentials from auth config
                let client_id = auth_config["client_id"].as_str().ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration("Missing client_id".to_string())
                })?;

                // Generate state token for CSRF protection
                let state = uuid::Uuid::new_v4().to_string();
                self.persist_oauth_state(&state, user_id, provider_id)
                    .await?;

                // Get external URL from config service
                let external_url = self
                    .config_service
                    .get_setting("external_url")
                    .await
                    .unwrap_or(None)
                    .ok_or_else(|| {
                        GitProviderManagerError::InvalidConfiguration(
                            "No external_url configured in settings".to_string(),
                        )
                    })?;

                // Construct redirect URI using external URL
                let redirect_uri = format!("{}/api/webhook/git/gitlab/auth", external_url);

                // Get GitLab instance URL from provider config (default to gitlab.com)
                // Note: We need the base URL, not the API URL for OAuth endpoints
                let gitlab_url = if let Some(api_url) = provider.api_url.as_ref() {
                    // If api_url contains "/api/v4", strip it to get the base URL
                    if api_url.contains("/api/v4") {
                        api_url
                            .replace("/api/v4", "")
                            .trim_end_matches('/')
                            .to_string()
                    } else {
                        api_url.trim_end_matches('/').to_string()
                    }
                } else {
                    "https://gitlab.com".to_string()
                };

                // Build GitLab OAuth authorization URL with proper scopes
                let auth_url = format!(
                    "{}/oauth/authorize?client_id={}&redirect_uri={}&response_type=code&state={}&scope={}",
                    gitlab_url,
                    client_id,
                    urlencoding::encode(&redirect_uri),
                    state,
                    urlencoding::encode(GITLAB_OAUTH_SCOPES)
                );

                Ok((auth_url, state))
            }
            GitProviderType::Gitea => {
                // Gitea does not support OAuth in v1. Each Gitea instance
                // requires its own OAuth app registration and there is no
                // central store. Use a Personal Access Token instead:
                // Settings > Applications > Access Tokens.
                Err(GitProviderManagerError::InvalidConfiguration(
                    "Gitea does not support OAuth in v1. \
                     Please create a Personal Access Token in your Gitea instance \
                     at Settings > Applications > Access Tokens and use the PAT flow."
                        .to_string(),
                ))
            }
            GitProviderType::Bitbucket => {
                // Bitbucket Cloud OAuth deferred to v2. Use Repository/Workspace
                // Access Tokens (RATs/WATs) or App Passwords via the PAT flow.
                Err(GitProviderManagerError::InvalidConfiguration(
                    "Bitbucket OAuth is not yet supported. \
                     Please use a Repository Access Token (RAT) or App Password \
                     from Bitbucket > Repository settings > Access tokens."
                        .to_string(),
                ))
            }
            GitProviderType::Generic => {
                // Generic/Manual providers do not support OAuth by definition.
                Err(GitProviderManagerError::InvalidConfiguration(
                    "Manual/Generic git providers do not support OAuth. \
                     Configure the clone URL and an optional HTTPS token instead."
                        .to_string(),
                ))
            }
        }
    }

    /// Handle OAuth callback for a git provider
    pub async fn handle_oauth_callback(
        &self,
        provider_id: i32,
        code: String,
        _state: String,
        user_id: i32,
        host_override: Option<String>,
    ) -> Result<git_provider_connections::Model, GitProviderManagerError> {
        let provider = self.get_provider(provider_id).await?;

        // Decrypt auth config to get OAuth credentials
        let auth_config = self.decrypt_sensitive_data(&provider.auth_config).await?;

        match GitProviderType::try_from(provider.provider_type.as_str())? {
            GitProviderType::GitHub => {
                let client_id = auth_config["client_id"].as_str().ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration("Missing client_id".to_string())
                })?;
                let client_secret = auth_config["client_secret"].as_str().ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "Missing client_secret".to_string(),
                    )
                })?;

                // Calculate redirect URI (must match the one used in start_oauth_flow)
                let redirect_uri = if let Some(host) = host_override {
                    format!("{}/git-providers/{}/callback", host, provider_id)
                } else if let Some(base_url) = &provider.base_url {
                    format!("{}/git-providers/{}/callback", base_url, provider_id)
                } else {
                    return Err(GitProviderManagerError::InvalidConfiguration(
                        "No base URL configured for OAuth callback".to_string(),
                    ));
                };

                // Exchange code for access token
                let client = reqwest::Client::new();
                let token_response = client
                    .post("https://github.com/login/oauth/access_token")
                    .header("Accept", "application/json")
                    .form(&[
                        ("client_id", client_id),
                        ("client_secret", client_secret),
                        ("code", &code),
                        ("redirect_uri", &redirect_uri),
                    ])
                    .send()
                    .await
                    .map_err(|e| {
                        GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                            "Failed to exchange code: {}",
                            e
                        )))
                    })?;

                let token_data: serde_json::Value = token_response.json().await.map_err(|e| {
                    GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                        "Failed to parse token response: {}",
                        e
                    )))
                })?;

                let access_token = token_data["access_token"].as_str().ok_or_else(|| {
                    GitProviderManagerError::ProviderError(GitProviderError::AuthenticationFailed(
                        "No access token in response".to_string(),
                    ))
                })?;
                debug!("OAuth token exchange completed successfully (token data redacted)");
                // Extract refresh token if present (GitHub OAuth apps with offline_access scope)
                let refresh_token = token_data["refresh_token"].as_str().map(|s| s.to_string());

                // Get user info from GitHub
                let user_response = client
                    .get("https://api.github.com/user")
                    .header("Authorization", format!("Bearer {}", access_token))
                    .header("User-Agent", "Temps-Engine")
                    .send()
                    .await
                    .map_err(|e| {
                        GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                            "Failed to get user info: {}",
                            e
                        )))
                    })?;

                let user_data: serde_json::Value = user_response.json().await.map_err(|e| {
                    GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                        "Failed to parse user response: {}",
                        e
                    )))
                })?;

                let account_name = user_data["login"]
                    .as_str()
                    .ok_or_else(|| {
                        GitProviderManagerError::ProviderError(GitProviderError::ApiError(
                            "No login in user data".to_string(),
                        ))
                    })?
                    .to_string();

                let account_type = if user_data["type"].as_str() == Some("Organization") {
                    "Organization".to_string()
                } else {
                    "User".to_string()
                };

                // Create or update connection
                self.create_connection(
                    provider_id,
                    user_id,
                    account_name,
                    account_type,
                    Some(access_token.to_string()),
                    refresh_token, // Pass the refresh token if present
                    None,          // No installation ID for OAuth
                    Some(user_data),
                    None,
                )
                .await
            }
            GitProviderType::GitLab => {
                let client_id = auth_config["client_id"].as_str().ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration("Missing client_id".to_string())
                })?;
                let client_secret = auth_config["client_secret"].as_str().ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "Missing client_secret".to_string(),
                    )
                })?;

                // Get external URL from config service (must match the one used in start_oauth_flow)
                let external_url = self
                    .config_service
                    .get_setting("external_url")
                    .await
                    .unwrap_or(None)
                    .ok_or_else(|| {
                        GitProviderManagerError::InvalidConfiguration(
                            "No external_url configured in settings".to_string(),
                        )
                    })?;

                // Construct redirect URI using external URL (must match the one used in start_oauth_flow)
                let redirect_uri = format!("{}/api/webhook/git/gitlab/auth", external_url);

                // Get GitLab instance URL from provider config (default to gitlab.com)
                // Note: We need the base URL, not the API URL for OAuth endpoints
                let gitlab_url = if let Some(api_url) = provider.api_url.as_ref() {
                    // If api_url contains "/api/v4", strip it to get the base URL
                    if api_url.contains("/api/v4") {
                        api_url
                            .replace("/api/v4", "")
                            .trim_end_matches('/')
                            .to_string()
                    } else {
                        api_url.trim_end_matches('/').to_string()
                    }
                } else {
                    "https://gitlab.com".to_string()
                };

                // Exchange code for access token. `gitlab_url` is the
                // operator/user-supplied self-hosted URL, so the client is
                // SSRF-guarded (validated + pinned DNS, no redirects).
                let client = crate::services::git_provider::guarded_client_builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()
                    .map_err(|e| {
                        GitProviderManagerError::ProviderError(GitProviderError::ApiError(
                            format!(
                                "Failed to build HTTP client for GitLab OAuth token exchange with {}: {}",
                                gitlab_url, e
                            ),
                        ))
                    })?;
                let token_url = format!("{}/oauth/token", gitlab_url);

                info!(
                    "GitLab OAuth token exchange - URL: {}, redirect_uri: {}",
                    token_url, redirect_uri
                );

                let token_response = client
                    .post(&token_url)
                    .form(&[
                        ("client_id", client_id),
                        ("client_secret", client_secret),
                        ("code", &code),
                        ("grant_type", "authorization_code"),
                        ("redirect_uri", &redirect_uri),
                    ])
                    .send()
                    .await
                    .map_err(|e| {
                        error!(
                            "Failed to send token exchange request to {}: {}",
                            token_url, e
                        );
                        GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                            "Failed to exchange code: {}",
                            e
                        )))
                    })?;

                let status = token_response.status();
                let response_text = token_response.text().await.map_err(|e| {
                    GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                        "Failed to read token response: {}",
                        e
                    )))
                })?;

                if !status.is_success() {
                    error!(
                        "GitLab token exchange failed with status {}: {}",
                        status, response_text
                    );
                    return Err(GitProviderManagerError::ProviderError(
                        GitProviderError::ApiError(format!(
                            "GitLab token exchange failed with status {}: {}",
                            status, response_text
                        )),
                    ));
                }

                let token_data: serde_json::Value =
                    serde_json::from_str(&response_text).map_err(|e| {
                        error!(
                            "Failed to parse GitLab token response as JSON: {}",
                            response_text
                        );
                        GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                            "Failed to parse token response: {}",
                            e
                        )))
                    })?;

                info!("GitLab token exchange successful");

                let access_token = token_data["access_token"].as_str().ok_or_else(|| {
                    GitProviderManagerError::ProviderError(GitProviderError::AuthenticationFailed(
                        "No access token in response".to_string(),
                    ))
                })?;

                // GitLab provides refresh token by default
                let refresh_token = token_data["refresh_token"].as_str().map(|s| s.to_string());

                // Get user info from GitLab - we need to add /api/v4 for API calls
                let gitlab_api_url = if !gitlab_url.contains("/api/v4") {
                    format!("{}/api/v4", gitlab_url)
                } else {
                    gitlab_url.clone()
                };
                let user_url = format!("{}/user", gitlab_api_url);
                info!("Getting GitLab user info from: {}", user_url);

                let user_response = client
                    .get(&user_url)
                    .header("Authorization", format!("Bearer {}", access_token))
                    .send()
                    .await
                    .map_err(|e| {
                        error!("Failed to get user info from {}: {}", user_url, e);
                        GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                            "Failed to get user info: {}",
                            e
                        )))
                    })?;

                let user_status = user_response.status();
                let user_text = user_response.text().await.map_err(|e| {
                    GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                        "Failed to read user response: {}",
                        e
                    )))
                })?;

                if !user_status.is_success() {
                    error!(
                        "GitLab user info request failed with status {}: {}",
                        user_status, user_text
                    );
                    return Err(GitProviderManagerError::ProviderError(
                        GitProviderError::ApiError(format!(
                            "GitLab user info request failed with status {}: {}",
                            user_status, user_text
                        )),
                    ));
                }

                let user_data: serde_json::Value =
                    serde_json::from_str(&user_text).map_err(|e| {
                        error!(
                            "Failed to parse GitLab user response as JSON: {}",
                            user_text
                        );
                        GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                            "Failed to parse user response: {}",
                            e
                        )))
                    })?;

                info!(
                    "GitLab user info retrieved successfully for user: {:?}",
                    user_data.get("username")
                );

                let account_name = user_data["username"]
                    .as_str()
                    .ok_or_else(|| {
                        GitProviderManagerError::ProviderError(GitProviderError::ApiError(
                            "No username in user data".to_string(),
                        ))
                    })?
                    .to_string();

                // GitLab doesn't have organization/user distinction in the same way
                // We'll use "User" as the default account type
                let account_type = "User".to_string();

                // Create or update connection
                self.create_connection(
                    provider_id,
                    user_id,
                    account_name,
                    account_type,
                    Some(access_token.to_string()),
                    refresh_token,
                    None, // No installation ID for OAuth
                    Some(user_data),
                    None,
                )
                .await
            }
            _ => Err(GitProviderManagerError::ProviderError(
                GitProviderError::NotImplemented,
            )),
        }
    }
}
