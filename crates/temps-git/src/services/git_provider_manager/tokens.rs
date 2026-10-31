// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Connection credentials: read, validate, refresh and persist access tokens,
//! mint scoped repository tokens, and encrypt or decrypt stored secrets.

use super::*;

impl GitProviderManager {
    fn url_has_origin(url: &str, expected_host: &str) -> bool {
        let Ok(url) = Url::parse(url) else {
            return false;
        };

        url.scheme() == "https"
            && url.host_str() == Some(expected_host)
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
    }

    /// Public-repository requests below are sent to github.com. A credential
    /// configured for GitHub Enterprise must never cross that provider origin.
    fn is_github_dot_com_provider(provider: &git_providers::Model) -> bool {
        if provider.provider_type != "github" && provider.provider_type != "github_app" {
            return false;
        }

        let api_is_public = provider
            .api_url
            .as_deref()
            .map(|url| Self::url_has_origin(url, "api.github.com"))
            .unwrap_or_else(|| {
                provider
                    .base_url
                    .as_deref()
                    .map(|url| Self::url_has_origin(url, "github.com"))
                    .unwrap_or(true)
            });
        let web_is_public = provider
            .base_url
            .as_deref()
            .map(|url| Self::url_has_origin(url, "github.com"))
            .unwrap_or(true);

        api_is_public && web_is_public
    }

    async fn get_active_connections_for_user(
        &self,
        user_id: i32,
    ) -> Result<Vec<git_provider_connections::Model>, sea_orm::DbErr> {
        git_provider_connections::Entity::find()
            .filter(git_provider_connections::Column::IsActive.eq(true))
            .filter(git_provider_connections::Column::IsExpired.eq(false))
            .filter(git_provider_connections::Column::UserId.eq(Some(user_id)))
            .order_by_desc(git_provider_connections::Column::UpdatedAt)
            .all(self.db.as_ref())
            .await
    }

    /// Decrypt an encrypted token string (e.g., a GitLab webhook signing token).
    /// Thin public wrapper around the private `decrypt_string` helper so that
    /// webhook handlers inside the same crate can access it without going through
    /// the full service layer.
    pub async fn decrypt_token(&self, encrypted: &str) -> Result<String, GitProviderManagerError> {
        self.decrypt_string(encrypted).await
    }

    /// Get a valid access token from one of this user's active GitHub connections.
    ///
    /// Public repository discovery uses this to receive GitHub's authenticated
    /// rate limit. Credentials are deliberately scoped to the requesting user:
    /// an unauthenticated request, deployment token, or another user must never
    /// borrow a connection token.
    pub async fn get_valid_github_token_for_user(&self, user_id: i32) -> Option<String> {
        use temps_entities::git_providers;

        let connections = self.get_active_connections_for_user(user_id).await.ok()?;

        for conn in connections {
            // Keep this defensive check even though the database query applies
            // the same ownership filter. It prevents an incorrectly-behaving
            // database mock or future query refactor from crossing tenants.
            if !conn.is_active || conn.is_expired || conn.user_id != Some(user_id) {
                continue;
            }

            // Check provider type is GitHub
            let provider = git_providers::Entity::find_by_id(conn.provider_id)
                .one(self.db.as_ref())
                .await
                .ok()
                .flatten();

            let Some(provider) = provider else {
                continue;
            };

            if !provider.is_active || !Self::is_github_dot_com_provider(&provider) {
                continue;
            }

            // get_connection_token validates expiry and refreshes OAuth/App tokens
            let token = match self.get_connection_token(conn.id).await {
                Ok(t) if !t.is_empty() => t,
                _ => continue,
            };

            // get_connection_token validates and, when supported, refreshes the
            // credential before returning it.
            tracing::debug!(
                user_id,
                connection_id = conn.id,
                "Using the request user's GitHub connection for public repository API calls"
            );
            return Some(token);
        }
        None
    }

    /// Get access token for a connection
    /// This method automatically validates and refreshes the token if needed
    pub async fn get_connection_token(
        &self,
        connection_id: i32,
    ) -> Result<String, GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;
        let provider = self.get_provider(connection.provider_id).await?;

        // For GitHub Apps, generate an installation token
        if provider.provider_type == "github" && provider.auth_method == "github_app" {
            // This is handled by validate_and_refresh_connection_token for GitHub Apps
            return self
                .validate_and_refresh_connection_token(connection_id)
                .await;
        }

        // Decrypt access token
        let access_token = if let Some(ref encrypted) = connection.access_token {
            self.decrypt_string(encrypted).await?
        } else {
            return Err(GitProviderManagerError::InvalidConfiguration(
                "No access token found".to_string(),
            ));
        };

        // Get the provider service
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Check if the token needs refresh
        if provider_service.token_needs_refresh(&access_token).await {
            // Token needs refresh, check if we have a refresh token
            if let Some(ref encrypted_refresh) = connection.refresh_token {
                tracing::info!(
                    "Access token needs refresh for connection {}, attempting to refresh",
                    connection_id
                );

                let refresh_token = self.decrypt_string(encrypted_refresh).await?;

                // Validate and refresh the token
                match provider_service
                    .validate_and_refresh_token(&access_token, Some(&refresh_token))
                    .await
                {
                    Ok((new_access_token, new_refresh_token)) => {
                        tracing::info!(
                            "Successfully refreshed token for connection {}",
                            connection_id
                        );

                        // Update the database with new tokens
                        // Calculate expiry (typically 1 hour for OAuth tokens)
                        let expires_at = chrono::Utc::now() + chrono::Duration::hours(1);

                        self.update_connection_tokens(
                            connection_id,
                            new_access_token.clone(),
                            new_refresh_token.or(Some(refresh_token)),
                            Some(expires_at),
                        )
                        .await?;

                        Ok(new_access_token)
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to refresh token for connection {}: {:?}",
                            connection_id,
                            e
                        );
                        // Return the original token, let the caller handle any authentication errors
                        Ok(access_token)
                    }
                }
            } else {
                tracing::debug!(
                    "Token needs refresh for connection {} but no refresh token available",
                    connection_id
                );

                // Check if this is a GitHub App installation token
                // GitHub Apps use installation tokens that can be regenerated
                if let Some(ref installation_id) = connection.installation_id {
                    tracing::info!(
                        "Connection {} is a GitHub App installation, generating new installation token",
                        connection_id
                    );

                    // Generate a new installation token
                    match provider_service
                        .validate_and_refresh_token("", Some(installation_id))
                        .await
                    {
                        Ok((new_access_token, _)) => {
                            tracing::info!(
                                "Successfully generated new installation token for connection {}",
                                connection_id
                            );

                            // Update the database with new token
                            let expires_at = chrono::Utc::now() + chrono::Duration::hours(1);

                            self.update_connection_tokens(
                                connection_id,
                                new_access_token.clone(),
                                None, // No refresh token for GitHub Apps
                                Some(expires_at),
                            )
                            .await?;

                            Ok(new_access_token)
                        }
                        Err(e) => {
                            tracing::error!(
                                "Failed to generate new installation token for connection {}: {:?}",
                                connection_id,
                                e
                            );
                            Err(GitProviderManagerError::InvalidConfiguration(format!(
                                "Failed to generate installation token: {}",
                                e
                            )))
                        }
                    }
                } else {
                    // No refresh token and not a GitHub App (e.g., Personal Access Token)
                    // Validate PAT by checking with the provider API
                    tracing::debug!(
                        "Connection {} appears to be a Personal Access Token, validating with provider API",
                        connection_id
                    );

                    match provider_service.validate_token(&access_token).await {
                        Ok(true) => {
                            // Token is valid, mark as not expired if it was previously marked
                            if connection.is_expired {
                                self.mark_connection_expired(connection_id, false).await?;
                            }
                            Ok(access_token)
                        }
                        Ok(false) | Err(_) => {
                            // Token validation failed, mark as expired
                            tracing::error!(
                                "PAT validation failed for connection {}, marking as expired",
                                connection_id
                            );
                            self.mark_connection_expired(connection_id, true).await?;

                            // Return specific error indicating the token is expired
                            Err(GitProviderManagerError::ConnectionTokenExpired { connection_id })
                        }
                    }
                }
            }
        } else {
            // Token is still valid
            Ok(access_token)
        }
    }

    /// Unconditionally refresh the OAuth/GitLab App access token for a
    /// connection, bypassing the expires_at check. Use this after a clone or
    /// download fails with an auth error: the stored token may have been
    /// revoked or invalidated server-side even if our recorded expiry says
    /// otherwise. Falls back to `validate_and_refresh_connection_token` for
    /// auth methods that don't use refresh tokens (PAT, GitHub App).
    pub async fn force_refresh_connection_token(
        &self,
        connection_id: i32,
    ) -> Result<String, GitProviderManagerError> {
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        let connection = self.get_connection(connection_id).await?;
        let provider = self.get_provider(connection.provider_id).await?;

        let auth_config = self.decrypt_sensitive_data(&provider.auth_config).await?;
        let auth_method = serde_json::from_value::<AuthMethod>(auth_config).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!("Invalid auth config: {}", e))
        })?;

        match auth_method {
            // OAuth / GitLab App: always exchange the refresh token for a new pair.
            AuthMethod::OAuth { .. } | AuthMethod::GitLabApp { .. } => {
                let refresh_encrypted = connection.refresh_token.as_ref().ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "Cannot force-refresh: no refresh token stored for connection".to_string(),
                    )
                })?;
                let refresh_token = self.decrypt_string(refresh_encrypted).await?;

                let access_token_old = match connection.access_token.as_ref() {
                    Some(enc) => self.decrypt_string(enc).await?,
                    None => String::new(),
                };

                info!(
                    "Force-refreshing OAuth/GitLabApp token for connection {} after auth failure",
                    connection_id
                );
                let provider_service = self.get_provider_service(provider.id).await?;
                let (new_access_token, new_refresh_token) = provider_service
                    .validate_and_refresh_token(&access_token_old, Some(&refresh_token))
                    .await?;

                let encrypted_access_token = self.encrypt_string(&new_access_token).await?;
                let mut active_connection: git_provider_connections::ActiveModel =
                    connection.into();
                active_connection.access_token = Set(Some(encrypted_access_token));
                if let Some(new_refresh) = new_refresh_token {
                    let encrypted_refresh = self.encrypt_string(&new_refresh).await?;
                    active_connection.refresh_token = Set(Some(encrypted_refresh));
                }
                active_connection.token_expires_at =
                    Set(Some(Utc::now() + chrono::Duration::hours(1)));
                active_connection.updated_at = Set(Utc::now());
                active_connection.update(self.db.as_ref()).await?;

                Ok(new_access_token)
            }
            // GitHub App installation tokens regenerate via JWT, not refresh
            // token. Delegate to the existing path which already handles that.
            AuthMethod::GitHubApp { .. } => {
                self.validate_and_refresh_connection_token(connection_id)
                    .await
            }
            // PATs can't be refreshed — surface the original token. Caller's
            // retry will fail again, which is the correct signal.
            AuthMethod::PersonalAccessToken { .. } => {
                self.validate_and_refresh_connection_token(connection_id)
                    .await
            }
            _ => Err(GitProviderManagerError::InvalidConfiguration(
                "Unsupported auth method for force-refresh".to_string(),
            )),
        }
    }

    /// Heuristically detect whether an error message represents an
    /// authentication/authorization failure that a token refresh might fix.
    /// Used by clone/download retry paths since the libgit2 + reqwest error
    /// surface doesn't expose HTTP status codes uniformly.
    pub(super) fn is_auth_failure(message: &str) -> bool {
        let lower = message.to_lowercase();
        lower.contains("401")
            || lower.contains("403")
            || lower.contains("unauthorized")
            || lower.contains("authentication failed")
            || lower.contains("authentication required")
            || lower.contains("invalid token")
            || lower.contains("invalid_token")
            || lower.contains("access token")
            || lower.contains("token expired")
    }

    /// Validate and refresh connection token if needed
    /// Returns the valid access token
    pub async fn validate_and_refresh_connection_token(
        &self,
        connection_id: i32,
    ) -> Result<String, GitProviderManagerError> {
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        let connection = self.get_connection(connection_id).await?;
        let provider = self.get_provider(connection.provider_id).await?;

        // Check auth method type
        let auth_config = self.decrypt_sensitive_data(&provider.auth_config).await?;
        let auth_method = serde_json::from_value::<AuthMethod>(auth_config).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!("Invalid auth config: {}", e))
        })?;

        // Check if this connection uses GitHub App installation token or PAT/OAuth
        // A connection is a GitHub App installation if:
        // 1. The provider is configured as GitHubApp AND
        // 2. The connection has an installation_id
        // Otherwise, treat it as PAT/OAuth (just return the stored token)
        let is_github_app_installation = matches!(auth_method, AuthMethod::GitHubApp { .. })
            && connection.installation_id.is_some();

        match auth_method {
            // GitHub App: Generate fresh installation token only if connection has installation_id
            AuthMethod::GitHubApp { .. } if is_github_app_installation => {
                // `is_github_app_installation` already required `Some`.
                let Some(installation_id) = connection.installation_id.as_ref() else {
                    return Err(GitProviderManagerError::InvalidConfiguration(format!(
                        "GitHub App connection {} has no installation_id",
                        connection_id
                    )));
                };

                debug!(
                    "Generating fresh installation token for GitHub App connection {}",
                    connection_id
                );

                let provider_service = self.get_provider_service(provider.id).await?;
                let new_access_token = provider_service
                    .validate_and_refresh_token("", Some(installation_id))
                    .await
                    .map(|(token, _)| token)?;

                // Save the new token
                let encrypted_access_token = self.encrypt_string(&new_access_token).await?;
                let mut active_connection: git_provider_connections::ActiveModel =
                    connection.into();
                active_connection.access_token = Set(Some(encrypted_access_token));
                active_connection.token_expires_at =
                    Set(Some(Utc::now() + chrono::Duration::hours(1)));
                active_connection.updated_at = Set(Utc::now());
                active_connection.update(self.db.as_ref()).await?;

                Ok(new_access_token)
            }

            // GitHub App provider but connection without installation_id (PAT connection)
            // Just return the stored token like a PAT
            AuthMethod::GitHubApp { .. } => {
                debug!(
                    "Connection {} uses GitHub App provider but has no installation_id, treating as PAT",
                    connection_id
                );
                let access_token = connection.access_token.as_ref().ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "No access token found".to_string(),
                    )
                })?;

                self.decrypt_string(access_token).await
            }

            // PAT: Just return the stored token (no refresh)
            AuthMethod::PersonalAccessToken { .. } => {
                let access_token = connection.access_token.as_ref().ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "No access token found".to_string(),
                    )
                })?;

                self.decrypt_string(access_token).await
            }

            // OAuth/GitLab App: Refresh if expired
            AuthMethod::OAuth { .. } | AuthMethod::GitLabApp { .. } => {
                let access_token = connection.access_token.as_ref().ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "No access token found".to_string(),
                    )
                })?;
                let access_token = self.decrypt_string(access_token).await?;

                // Check if expired (with 5 minute buffer). The previous 60-second
                // buffer was not enough headroom: a slow clone over flaky
                // network can easily run past the wire-time check before the
                // libgit2 credential callback even fires, leaving us with a
                // freshly-validated-but-just-expired token. Five minutes makes
                // that window much harder to hit while still letting us reuse
                // tokens for nearly their entire lifetime.
                let should_refresh = if let Some(expires_at) = connection.token_expires_at {
                    let now = Utc::now();
                    let buffer = chrono::Duration::minutes(5);
                    expires_at < now + buffer
                } else {
                    false
                };

                if !should_refresh {
                    return Ok(access_token);
                }

                // Get refresh token
                let refresh_token = connection.refresh_token.as_ref().ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "Token expired but no refresh token available".to_string(),
                    )
                })?;
                let refresh_token = self.decrypt_string(refresh_token).await?;

                // Refresh the token
                info!("Refreshing OAuth token for connection {}", connection_id);
                let provider_service = self.get_provider_service(provider.id).await?;
                let (new_access_token, new_refresh_token) = provider_service
                    .validate_and_refresh_token(&access_token, Some(&refresh_token))
                    .await?;

                // Save new tokens
                let encrypted_access_token = self.encrypt_string(&new_access_token).await?;
                let mut active_connection: git_provider_connections::ActiveModel =
                    connection.into();
                active_connection.access_token = Set(Some(encrypted_access_token));

                if let Some(new_refresh) = new_refresh_token {
                    let encrypted_refresh = self.encrypt_string(&new_refresh).await?;
                    active_connection.refresh_token = Set(Some(encrypted_refresh));
                }

                active_connection.token_expires_at =
                    Set(Some(Utc::now() + chrono::Duration::hours(1)));
                active_connection.updated_at = Set(Utc::now());
                active_connection.update(self.db.as_ref()).await?;

                Ok(new_access_token)
            }

            _ => Err(GitProviderManagerError::InvalidConfiguration(
                "Unsupported auth method".to_string(),
            )),
        }
    }

    /// Mint a per-operation, single-repo, narrow-permission credential.
    ///
    /// Wraps [`GitProviderService::mint_scoped_repo_token`] with the
    /// connection lookup so callers (the workspace credential daemon's
    /// HTTP endpoint, today the only caller) need only know the
    /// `connection_id` — not the underlying provider type, installation
    /// id, or auth method.
    ///
    /// The returned [`ScopedTokenGrant`] is shaped for direct use as
    /// `username:password` in a `git clone` URL. Tokens live ≤1 hour
    /// (GitHub's hard cap on installation tokens) and are scoped to
    /// `(owner, repo, operation)` only — even leaked, the blast radius
    /// is bounded to one repo with one permission for one hour.
    ///
    /// PAT and OAuth connections fail with [`GitProviderManagerError::InvalidConfiguration`]
    /// because there's no provider API to narrow them at runtime. The
    /// daemon should refuse those requests rather than handing out the
    /// stored long-lived token.
    pub async fn mint_scoped_repo_token_for_connection(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        operation: crate::services::git_provider::ScopedTokenOp,
    ) -> Result<crate::services::git_provider::ScopedTokenGrant, GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;

        // Fast path: GitHub App installation. Provider service mints a
        // fresh per-op installation token narrowed to (repo, permission)
        // — the security model the daemon was originally designed for.
        if let Some(installation_id) = connection.installation_id.as_deref() {
            let provider_service = self.get_provider_service(connection.provider_id).await?;
            return provider_service
                .mint_scoped_repo_token(Some(installation_id), owner, repo, operation)
                .await
                .map_err(GitProviderManagerError::from);
        }

        Err(GitProviderManagerError::InvalidConfiguration(format!(
            "Connection {} is not backed by a GitHub App installation; PAT/OAuth connections cannot mint scoped per-op tokens",
            connection_id
        )))
    }

    /// Get decrypted webhook secret for a provider
    pub async fn get_webhook_secret(
        &self,
        provider_id: i32,
    ) -> Result<Option<String>, GitProviderManagerError> {
        let provider = self.get_provider(provider_id).await?;

        // Decrypt webhook_secret if present
        if let Some(ref encrypted) = provider.webhook_secret {
            Ok(Some(self.decrypt_string(encrypted).await?))
        } else {
            Ok(None)
        }
    }

    /// Update connection tokens
    pub async fn update_connection_tokens(
        &self,
        connection_id: i32,
        access_token: String,
        refresh_token: Option<String>,
        expires_at: Option<UtcDateTime>,
    ) -> Result<(), GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;

        let encrypted_access = self.encrypt_string(&access_token).await?;
        let encrypted_refresh = if let Some(token) = refresh_token {
            Some(self.encrypt_string(&token).await?)
        } else {
            None
        };

        let mut active_model: git_provider_connections::ActiveModel = connection.into();
        active_model.access_token = Set(Some(encrypted_access));
        if encrypted_refresh.is_some() {
            active_model.refresh_token = Set(encrypted_refresh);
        }
        active_model.token_expires_at = Set(expires_at);
        active_model.last_synced_at = Set(Some(chrono::Utc::now()));

        active_model.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Execute an API call with automatic token refresh on authentication failure
    /// This method wraps API calls to Git providers and automatically refreshes the token if it fails with 401
    ///
    /// # Arguments
    /// * `connection_id` - The ID of the Git provider connection
    /// * `api_call` - A closure that performs the API call using the access token
    ///
    /// # Returns
    /// The result of the API call, with automatic retry after token refresh if needed
    pub(super) async fn execute_with_refresh<F, Fut, T>(
        &self,
        connection_id: i32,
        api_call: F,
    ) -> Result<T, GitProviderManagerError>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<
            Output = Result<T, crate::services::git_provider::GitProviderError>,
        >,
    {
        // Get the connection
        let connection = self.get_connection(connection_id).await?;

        // Get the provider service
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Get current access token
        let access_token = if let Some(ref encrypted) = connection.access_token {
            self.decrypt_string(encrypted).await?
        } else {
            return Err(GitProviderManagerError::InvalidConfiguration(
                "No access token found".to_string(),
            ));
        };

        // First, check if the token needs refresh proactively
        if provider_service.token_needs_refresh(&access_token).await {
            if let Some(ref encrypted_refresh) = connection.refresh_token {
                tracing::info!(
                    "Access token needs refresh for connection {}, attempting to refresh proactively",
                    connection_id
                );

                let refresh_token = self.decrypt_string(encrypted_refresh).await?;

                // Validate and refresh the token
                match provider_service
                    .validate_and_refresh_token(&access_token, Some(&refresh_token))
                    .await
                {
                    Ok((new_access_token, new_refresh_token)) => {
                        tracing::info!(
                            "Successfully refreshed token proactively for connection {}",
                            connection_id
                        );

                        // Update the database with new tokens
                        // Calculate expiry (typically 1 hour for OAuth tokens)
                        let expires_at = chrono::Utc::now() + chrono::Duration::hours(1);

                        self.update_connection_tokens(
                            connection_id,
                            new_access_token.clone(),
                            new_refresh_token.or(Some(refresh_token)),
                            Some(expires_at),
                        )
                        .await?;

                        // Use the new token for the API call
                        return api_call(new_access_token).await.map_err(Into::into);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to refresh token proactively for connection {}: {:?}",
                            connection_id,
                            e
                        );
                        // Continue with the original token and handle failure reactively
                    }
                }
            }
        }

        // Try the API call with the current token
        match api_call(access_token.clone()).await {
            Ok(result) => Ok(result),
            Err(e) if self.is_authentication_error(&e) => {
                // Token might be expired, try to refresh if we have a refresh token
                if let Some(ref encrypted_refresh) = connection.refresh_token {
                    tracing::info!(
                        "Access token expired for connection {} (got 401), attempting to refresh",
                        connection_id
                    );

                    let refresh_token = self.decrypt_string(encrypted_refresh).await?;

                    // Validate and refresh the token
                    match provider_service
                        .validate_and_refresh_token(&access_token, Some(&refresh_token))
                        .await
                    {
                        Ok((new_access_token, new_refresh_token)) => {
                            tracing::info!(
                                "Successfully refreshed token after 401 for connection {}",
                                connection_id
                            );

                            // Update the database with new tokens
                            // Calculate expiry (typically 1 hour for OAuth tokens)
                            let expires_at = chrono::Utc::now() + chrono::Duration::hours(1);

                            self.update_connection_tokens(
                                connection_id,
                                new_access_token.clone(),
                                new_refresh_token.or(Some(refresh_token)),
                                Some(expires_at),
                            )
                            .await?;

                            // Retry the API call with the new token
                            api_call(new_access_token).await.map_err(Into::into)
                        }
                        Err(refresh_error) => {
                            tracing::error!(
                                "Failed to refresh token for connection {}: {:?}",
                                connection_id,
                                refresh_error
                            );
                            Err(refresh_error.into())
                        }
                    }
                } else {
                    tracing::warn!(
                        "Access token expired for connection {} but no refresh token available",
                        connection_id
                    );
                    Err(e.into())
                }
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Check if a GitProviderError is an authentication error (401)
    fn is_authentication_error(
        &self,
        error: &crate::services::git_provider::GitProviderError,
    ) -> bool {
        matches!(
            error,
            crate::services::git_provider::GitProviderError::AuthenticationFailed(_)
        )
    }

    pub(super) async fn encrypt_string(
        &self,
        data: &str,
    ) -> Result<String, GitProviderManagerError> {
        self.encryption_service.encrypt_string(data).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!("Encryption failed: {}", e))
        })
    }

    pub(super) async fn decrypt_string(
        &self,
        data: &str,
    ) -> Result<String, GitProviderManagerError> {
        self.encryption_service.decrypt_string(data).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!("Decryption failed: {}", e))
        })
    }

    pub(super) async fn encrypt_sensitive_data(
        &self,
        data: &serde_json::Value,
    ) -> Result<serde_json::Value, GitProviderManagerError> {
        // For now, just return the data as-is
        // In production, encrypt sensitive fields
        Ok(data.clone())
    }

    pub async fn decrypt_sensitive_data(
        &self,
        data: &serde_json::Value,
    ) -> Result<serde_json::Value, GitProviderManagerError> {
        // For now, just return the data as-is
        // In production, decrypt sensitive fields
        Ok(data.clone())
    }

    /// Update access token for a connection (for when tokens expire or are rotated)
    /// Validates the new token before updating the database
    pub async fn update_connection_token(
        &self,
        connection_id: i32,
        new_access_token: String,
        new_refresh_token: Option<String>,
    ) -> Result<(), GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;

        // When updating a connection token, we're providing a PAT, so validate it as a PAT
        // using the /user endpoint, not the /installation/repositories endpoint
        // This allows users to update a GitHub App installation connection to use a PAT
        let provider = self.get_provider(connection.provider_id).await?;

        // Validate the new access token as a PAT. GitHub's `/rate_limit`
        // authenticates repository-scoped and Actions credentials without
        // requiring user-profile access; GitLab still uses `/user`.
        //
        // `api_url` may be a self-hosted instance the user configured, so
        // this probe uses the SSRF-guarded client (validated + pinned DNS,
        // no redirects) like the provider services do.
        let client = crate::services::git_provider::guarded_client_builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| {
                GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                    "Failed to build HTTP client to validate the token for connection {}: {}",
                    connection_id, e
                )))
            })?;
        let api_url = provider
            .api_url
            .as_deref()
            .unwrap_or("https://api.github.com");
        let validation_endpoint =
            if provider.provider_type == "github" || provider.provider_type == "gitlab" {
                if provider.provider_type == "github" {
                    format!("{}/rate_limit", api_url)
                } else {
                    format!("{}/user", api_url)
                }
            } else {
                // For other providers, use the provider service's validate_token
                let provider_service = self.get_provider_service(connection.provider_id).await?;
                match provider_service.validate_token(&new_access_token).await {
                    Ok(true) => {
                        tracing::info!(
                            "New access token validated successfully for connection {}",
                            connection_id
                        );
                        // Continue to save the token
                        return self
                            .save_updated_connection_token(
                                connection,
                                new_access_token,
                                new_refresh_token,
                                connection_id,
                                false, // Don't clear installation_id for non-GitHub/GitLab providers
                            )
                            .await;
                    }
                    Ok(false) => {
                        return Err(GitProviderManagerError::InvalidConfiguration(
                            "The provided access token is invalid or has insufficient permissions"
                                .to_string(),
                        ));
                    }
                    Err(e) => {
                        return Err(GitProviderManagerError::InvalidConfiguration(format!(
                            "Failed to validate the provided access token: {}",
                            e
                        )));
                    }
                }
            };

        let response = client
            .get(&validation_endpoint)
            .header("Authorization", format!("Bearer {}", new_access_token))
            .header("User-Agent", "Temps-Git-Provider")
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| {
                GitProviderManagerError::InvalidConfiguration(format!(
                    "Failed to validate token: {}",
                    e
                ))
            })?;

        match response.status() {
            status if status.is_success() => {
                tracing::info!(
                    "New access token validated successfully for connection {}",
                    connection_id
                );
            }
            status if status.as_u16() == 401 => {
                tracing::warn!(
                    "New access token validation failed for connection {} - token appears invalid (401)",
                    connection_id
                );
                return Err(GitProviderManagerError::InvalidConfiguration(
                    "The provided access token is invalid or has insufficient permissions"
                        .to_string(),
                ));
            }
            status => {
                let provider_message = response
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|body| {
                        body.get("message")
                            .and_then(|value| value.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| format!("HTTP {}", status));
                tracing::warn!(
                    "Error validating new access token for connection {}: {} - {}",
                    connection_id,
                    status,
                    provider_message
                );
                let error = match status {
                    reqwest::StatusCode::FORBIDDEN => GitProviderError::PermissionDenied {
                        operation: "validate the replacement credential".to_string(),
                        required_permission: if provider.provider_type == "github" {
                            "Metadata: read".to_string()
                        } else {
                            "read_user".to_string()
                        },
                        provider_message,
                    },
                    reqwest::StatusCode::TOO_MANY_REQUESTS => GitProviderError::RateLimitExceeded,
                    _ => GitProviderError::ApiError(format!(
                        "{} returned HTTP {} while validating the replacement credential: {}",
                        provider.provider_type, status, provider_message
                    )),
                };
                return Err(error.into());
            }
        }

        // If the connection had an installation_id and we're now using a PAT, clear it
        let clear_installation_id = connection.installation_id.is_some();
        if clear_installation_id {
            tracing::info!(
                "Connection {} is being updated from GitHub App installation to PAT, clearing installation_id",
                connection_id
            );
        }

        self.save_updated_connection_token(
            connection,
            new_access_token,
            new_refresh_token,
            connection_id,
            clear_installation_id,
        )
        .await
    }

    /// Helper method to save updated connection token
    async fn save_updated_connection_token(
        &self,
        connection: git_provider_connections::Model,
        new_access_token: String,
        new_refresh_token: Option<String>,
        connection_id: i32,
        clear_installation_id: bool,
    ) -> Result<(), GitProviderManagerError> {
        // Encrypt the new tokens
        let encrypted_access = self.encrypt_string(&new_access_token).await?;
        let encrypted_refresh = if let Some(token) = new_refresh_token {
            Some(self.encrypt_string(&token).await?)
        } else {
            None
        };

        let mut active_model: git_provider_connections::ActiveModel = connection.into();
        active_model.access_token = Set(Some(encrypted_access));
        if encrypted_refresh.is_some() {
            active_model.refresh_token = Set(encrypted_refresh);
        }
        active_model.token_expires_at = Set(None); // Reset expiration since we have a new token
        active_model.updated_at = Set(chrono::Utc::now());
        active_model.is_active = Set(true); // Reactivate if it was deactivated due to expired token
        active_model.is_expired = Set(false); // Reset expired flag since we have a validated token

        // Clear installation_id if switching from GitHub App to PAT
        if clear_installation_id {
            active_model.installation_id = Set(None);
        }

        active_model.update(self.db.as_ref()).await?;

        tracing::info!(
            "Successfully updated and validated access token for connection {}",
            connection_id
        );
        Ok(())
    }

    /// Mark a connection as expired or not expired
    pub async fn mark_connection_expired(
        &self,
        connection_id: i32,
        is_expired: bool,
    ) -> Result<(), GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;

        let mut active_model: git_provider_connections::ActiveModel = connection.into();
        active_model.is_expired = Set(is_expired);
        active_model.updated_at = Set(chrono::Utc::now());

        active_model.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Check if a connection's token is expired and mark it inactive if so
    pub async fn check_token_expiration(
        &self,
        connection_id: i32,
    ) -> Result<bool, GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;

        // Check if token has expiration date and if it's expired
        if let Some(expires_at) = connection.token_expires_at {
            if expires_at < chrono::Utc::now() {
                // Token is expired, mark connection as inactive
                self.deactivate_connection(connection_id).await?;
                return Ok(true);
            }
        }

        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::git_provider_manager::test_support::*;
    use temps_database::test_utils::TestDatabase;

    #[test]
    fn public_github_token_selection_rejects_enterprise_origins() {
        assert!(GitProviderManager::is_github_dot_com_provider(
            &provider_model(
                "github",
                Some("https://github.com/acme"),
                Some("https://api.github.com")
            )
        ));
        assert!(GitProviderManager::is_github_dot_com_provider(
            &provider_model("github_app", None, None)
        ));

        for provider in [
            provider_model(
                "github",
                Some("https://github.enterprise.example"),
                Some("https://github.enterprise.example/api/v3"),
            ),
            provider_model(
                "github_app",
                Some("https://github.enterprise.example"),
                Some("https://api.github.com"),
            ),
            provider_model(
                "github",
                Some("https://github.com.evil.example"),
                Some("https://api.github.com.evil.example"),
            ),
        ] {
            assert!(
                !GitProviderManager::is_github_dot_com_provider(&provider),
                "a non-GitHub.com credential must be rejected before token decryption or validation"
            );
        }
    }

    #[tokio::test]
    async fn request_scoped_connection_lookup_isolates_users_and_inactive_rows() {
        use temps_entities::users;

        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!("Postgres unavailable; skipping connection-isolation test: {error}");
                return;
            }
        };
        let db = test_db.connection_arc();
        let now = chrono::Utc::now();

        let make_user = |email: &str| users::ActiveModel {
            email: Set(email.to_string()),
            password_hash: Set(Some("hash".to_string())),
            name: Set(email.to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        let user_a = make_user("github-token-a@example.com")
            .insert(db.as_ref())
            .await
            .unwrap();
        let user_b = make_user("github-token-b@example.com")
            .insert(db.as_ref())
            .await
            .unwrap();

        let provider = git_providers::ActiveModel {
            name: Set("GitHub.com".to_string()),
            provider_type: Set("github".to_string()),
            base_url: Set(Some("https://github.com".to_string())),
            api_url: Set(Some("https://api.github.com".to_string())),
            auth_method: Set("pat".to_string()),
            auth_config: Set(serde_json::json!({})),
            webhook_secret: Set(None),
            is_active: Set(true),
            is_default: Set(false),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .unwrap();

        for (user_id, account_name, is_active) in [
            (Some(user_a.id), "user-a-active", true),
            (Some(user_a.id), "user-a-inactive", false),
            (Some(user_b.id), "user-b-active", true),
            (None, "ownerless-active", true),
        ] {
            git_provider_connections::ActiveModel {
                provider_id: Set(provider.id),
                user_id: Set(user_id),
                account_name: Set(account_name.to_string()),
                account_type: Set("User".to_string()),
                access_token: Set(None),
                refresh_token: Set(None),
                token_expires_at: Set(None),
                refresh_token_expires_at: Set(None),
                installation_id: Set(None),
                metadata: Set(None),
                is_active: Set(is_active),
                is_expired: Set(false),
                syncing: Set(false),
                last_synced_at: Set(None),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .unwrap();
        }

        let manager = GitProviderManager::new(
            db.clone(),
            Arc::new(
                temps_core::EncryptionService::new(
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                )
                .unwrap(),
            ),
            Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
            create_test_config_service(db),
        );

        let connections = manager
            .get_active_connections_for_user(user_a.id)
            .await
            .unwrap();
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].account_name, "user-a-active");
    }

    #[test]
    fn is_auth_failure_recognizes_common_messages() {
        // Things that should trigger a retry-with-fresh-token.
        assert!(GitProviderManager::is_auth_failure(
            "HTTP 401: Unauthorized"
        ));
        assert!(GitProviderManager::is_auth_failure("HTTP 403"));
        assert!(GitProviderManager::is_auth_failure(
            "remote: HTTP authentication required"
        ));
        assert!(GitProviderManager::is_auth_failure(
            "authentication failed for 'https://gitlab.com/foo/bar.git'"
        ));
        assert!(GitProviderManager::is_auth_failure(
            "GitLab error: invalid_token"
        ));
        assert!(GitProviderManager::is_auth_failure(
            "the provided access token is invalid"
        ));
        assert!(GitProviderManager::is_auth_failure(
            "HTTP 401: token expired"
        ));

        // Things that should NOT trigger a retry — refreshing won't help.
        assert!(!GitProviderManager::is_auth_failure("HTTP 404: not found"));
        assert!(!GitProviderManager::is_auth_failure(
            "connection refused (ECONNREFUSED)"
        ));
        assert!(!GitProviderManager::is_auth_failure("directory not empty"));
        assert!(!GitProviderManager::is_auth_failure("disk full"));
    }

    #[tokio::test]
    async fn public_github_token_uses_valid_connection_owned_by_request_user() {
        use crate::services::git_provider::{AuthMethod, GitProviderService};
        use crate::services::github_provider::GitHubProvider;
        use sea_orm::{DatabaseBackend, MockDatabase};

        let mut server = mockito::Server::new_async().await;
        let validate_token = server
            .mock("GET", "/rate_limit")
            .match_header("authorization", "Bearer owned-github-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"resources":{"core":{"remaining":4999}}}"#)
            .create_async()
            .await;

        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test encryption key should be valid"),
        );
        let mut connection = connection_fixture(11, Some(5));
        connection.access_token = Some(
            encryption_service
                .encrypt_string("owned-github-token")
                .expect("test token should encrypt"),
        );
        let provider_model = github_provider_fixture();
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[connection.clone()]])
                .append_query_results([[provider_model.clone()]])
                .append_query_results([[connection]])
                .append_query_results([[provider_model]])
                .into_connection(),
        );
        let manager = GitProviderManager::new(
            db.clone(),
            encryption_service,
            Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
            create_test_config_service(db),
        );
        let provider: Arc<dyn GitProviderService> = Arc::new(GitHubProvider::new(
            Some(server.url()),
            AuthMethod::PersonalAccessToken {
                token: "unused-constructor-token".to_string(),
            },
        ));
        manager.providers_cache.write().await.insert(7, provider);

        let token = manager.get_valid_github_token_for_user(5).await;

        assert_eq!(token.as_deref(), Some("owned-github-token"));
        validate_token.assert_async().await;
    }

    #[tokio::test]
    async fn public_github_token_never_uses_another_users_connection() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        // MockDatabase returns queued rows without applying SQL predicates.
        // Returning another user's row exercises the method's defensive
        // ownership check in addition to the query's UserId filter.
        let connection = connection_fixture(11, Some(6));
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([[connection]])
                .into_connection(),
        );
        let manager = GitProviderManager::new(
            db.clone(),
            Arc::new(
                temps_core::EncryptionService::new(
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                )
                .expect("test encryption key should be valid"),
            ),
            Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
            create_test_config_service(db),
        );

        assert_eq!(manager.get_valid_github_token_for_user(5).await, None);
    }

    #[tokio::test]
    async fn scoped_repo_tokens_refuse_pat_connections() {
        use crate::services::git_provider::ScopedTokenOp;
        use chrono::Utc;
        use sea_orm::{DatabaseBackend, MockDatabase};

        let raw_token = "ghp_RAW_LONG_LIVED_PAT_SCOPE_ALL_REPOS";
        let now = Utc::now();
        let connection = git_provider_connections::Model {
            id: 42,
            provider_id: 7,
            user_id: Some(1),
            account_name: "pat-account".to_string(),
            account_type: "User".to_string(),
            access_token: Some(raw_token.to_string()),
            refresh_token: None,
            token_expires_at: None,
            refresh_token_expires_at: None,
            installation_id: None,
            metadata: None,
            is_active: true,
            is_expired: false,
            syncing: false,
            last_synced_at: None,
            synced_repository_count: 0,
            health_status: "unknown".to_string(),
            health_message: None,
            last_health_check_at: None,
            consecutive_health_failures: 0,
            last_sync_error: None,
            last_sync_error_at: None,
            created_at: now,
            updated_at: now,
        };
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![connection.clone()]])
                .into_connection(),
        );
        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );
        let manager = GitProviderManager::new(
            db.clone(),
            encryption_service,
            Arc::new(MockJobQueue) as Arc<dyn JobQueue>,
            create_test_config_service(db),
        );

        let error = manager
            .mint_scoped_repo_token_for_connection(
                connection.id,
                "owner",
                "repo",
                ScopedTokenOp::Fetch,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                GitProviderManagerError::InvalidConfiguration(ref reason)
                    if reason.contains("PAT/OAuth connections cannot mint scoped per-op tokens")
            ),
            "unexpected error: {error:?}"
        );
        assert!(!error.to_string().contains(raw_token));
    }
}
