// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Git provider records: create, read, update credentials, (de)activate and
//! delete providers, and build the cached provider service for one.

use super::*;

impl GitProviderManager {
    /// Get provider service by ID
    pub async fn get_provider_service(
        &self,
        provider_id: i32,
    ) -> Result<Arc<dyn GitProviderService>, GitProviderManagerError> {
        // Check cache first
        if let Some(service) = self.providers_cache.read().await.get(&provider_id) {
            return Ok(service.clone());
        }

        // Load provider from DB
        let provider = self.get_provider(provider_id).await?;

        // Decrypt auth config
        let auth_config_json = self.decrypt_sensitive_data(&provider.auth_config).await?;
        let auth_method: AuthMethod = serde_json::from_value(auth_config_json)?;

        // Create provider service
        let provider_type = GitProviderType::try_from(provider.provider_type.as_str())?;
        let service = GitProviderFactory::create_provider(
            provider_type,
            auth_method,
            provider.base_url.clone(),
            provider.api_url.clone(),
            self.db.clone(),
        )
        .await?;

        // Cache and return
        let service_arc: Arc<dyn GitProviderService> = Arc::from(service);
        self.providers_cache
            .write()
            .await
            .insert(provider_id, service_arc.clone());

        Ok(service_arc)
    }

    /// Create a new git provider configuration
    #[allow(clippy::too_many_arguments)]
    pub async fn create_provider(
        &self,
        name: String,
        provider_type: GitProviderType,
        auth_method: AuthMethod,
        base_url: Option<String>,
        api_url: Option<String>,
        webhook_secret: Option<String>,
        is_default: bool,
    ) -> Result<git_providers::Model, GitProviderManagerError> {
        // If setting as default, unset other defaults
        if is_default {
            let _ = self.unset_default_providers().await;
        }

        // Serialize and encrypt auth config
        let auth_config_json = serde_json::to_value(&auth_method)?;
        let encrypted_config = self.encrypt_sensitive_data(&auth_config_json).await?;

        // Encrypt webhook_secret if provided
        let encrypted_webhook_secret = if let Some(secret) = webhook_secret {
            Some(self.encrypt_string(&secret).await?)
        } else {
            None
        };

        let new_provider = git_providers::ActiveModel {
            name: Set(name),
            provider_type: Set(provider_type.to_string()),
            base_url: Set(base_url),
            api_url: Set(api_url),
            auth_method: Set(self.get_auth_method_type(&auth_method)),
            auth_config: Set(encrypted_config),
            webhook_secret: Set(encrypted_webhook_secret),
            is_active: Set(true),
            is_default: Set(is_default),
            ..Default::default()
        };

        let provider = new_provider.insert(self.db.as_ref()).await?;

        // Clear cache to force reload
        self.providers_cache.write().await.clear();

        Ok(provider)
    }

    /// Get all configured providers
    pub async fn list_providers(
        &self,
    ) -> Result<Vec<git_providers::Model>, GitProviderManagerError> {
        let providers = git_providers::Entity::find()
            .filter(git_providers::Column::IsActive.eq(true))
            .order_by_desc(git_providers::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(providers)
    }

    /// Get a specific provider
    pub async fn get_provider(
        &self,
        provider_id: i32,
    ) -> Result<git_providers::Model, GitProviderManagerError> {
        let provider = git_providers::Entity::find_by_id(provider_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| GitProviderManagerError::ProviderNotFound(provider_id.to_string()))?;

        Ok(provider)
    }

    /// Get the default provider
    pub async fn get_default_provider(
        &self,
    ) -> Result<Option<git_providers::Model>, GitProviderManagerError> {
        let provider = git_providers::Entity::find()
            .filter(git_providers::Column::IsDefault.eq(true))
            .filter(git_providers::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?;

        Ok(provider)
    }

    /// Create a git provider entry for a GitHub installation automatically
    pub async fn create_github_installation_provider(
        &self,
        installation_id: i32,
        account_name: String,
        account_type: String,
        access_token: String,
        html_url: Option<String>,
    ) -> Result<git_providers::Model, GitProviderManagerError> {
        // Create provider name based on account
        let provider_name = format!("GitHub App - {}", account_name);

        // Create auth method for GitHub installation
        let auth_method = AuthMethod::GitHubApp {
            app_id: 0, // Will be populated from installation data
            client_id: String::new(),
            client_secret: String::new(),
            private_key: String::new(),
            webhook_secret: String::new(),
        };

        // Create the git provider entry
        let provider = self
            .create_provider(
                provider_name,
                GitProviderType::GitHub,
                auth_method,
                html_url, // Base URL for web interface (from GitHub App data)
                Some("https://api.github.com".to_string()), // API URL for API calls
                None,     // webhook_secret handled by system-level GitHub App
                false,    // not default unless it's the first one
            )
            .await?;

        // Create a connection for this installation
        self.create_connection(
            provider.id,
            0, // System user, it's org-level
            account_name,
            account_type,
            Some(access_token),
            None, // No refresh token for installations
            Some(installation_id.to_string()),
            None, // No additional metadata for now
            None,
        )
        .await?;

        Ok(provider)
    }

    /// Create a git provider entry for GitHub PAT
    pub async fn create_github_pat_provider(
        &self,
        name: String,
        pat_token: String,
        user_id: i32,
    ) -> Result<git_providers::Model, GitProviderManagerError> {
        let auth_method = AuthMethod::PersonalAccessToken {
            token: pat_token.clone(),
        };

        let provider = self
            .create_provider(
                name,
                GitProviderType::GitHub,
                auth_method.clone(),
                Some("https://github.com".to_string()), // Base URL for web interface
                Some("https://api.github.com".to_string()), // API URL for API calls
                None,
                false,
            )
            .await?;

        // Get the actual GitHub username using the PAT
        let provider_service = GitProviderFactory::create_provider(
            GitProviderType::GitHub,
            auth_method,
            Some("https://api.github.com".to_string()),
            Some("https://api.github.com".to_string()),
            self.db.clone(),
        )
        .await?;

        // Get user info to determine the actual account name
        let user_info = provider_service.get_user(&pat_token).await?;

        // Create a connection for this PAT with the actual username
        let connection = self
            .create_connection(
                provider.id,
                user_id,
                user_info.username, // Use actual GitHub username
                "User".to_string(),
                Some(pat_token),
                None,
                None,
                None,
                None,
            )
            .await?;

        // Kick off sync in the background so creating the provider returns
        // quickly. The sync's own guard resets `syncing` on any exit path.
        let manager = Arc::new(self.clone());
        if let Err(e) = manager.spawn_sync_repositories(connection.id).await {
            tracing::warn!(
                "Failed to start background sync for GitHub PAT connection {}: {}",
                connection.id,
                e
            );
            // Don't fail the provider creation if sync can't be kicked off.
        }

        Ok(provider)
    }

    /// Create a git provider entry for GitLab PAT
    pub async fn create_gitlab_pat_provider(
        &self,
        name: String,
        pat_token: String,
        user_id: i32,
        base_url: Option<String>,
    ) -> Result<git_providers::Model, GitProviderManagerError> {
        let auth_method = AuthMethod::PersonalAccessToken {
            token: pat_token.clone(),
        };

        let api_url = base_url
            .as_ref()
            .map(|url| format!("{}/api/v4", url))
            .unwrap_or_else(|| "https://gitlab.com/api/v4".to_string());

        let provider = self
            .create_provider(
                name,
                GitProviderType::GitLab,
                auth_method.clone(),
                base_url
                    .clone()
                    .or_else(|| Some("https://gitlab.com".to_string())), // Base URL for web interface
                Some(api_url.clone()), // API URL for API calls
                None,
                false,
            )
            .await?;

        // Get the actual GitLab username using the PAT
        let provider_service = GitProviderFactory::create_provider(
            GitProviderType::GitLab,
            auth_method,
            base_url,
            Some(api_url),
            self.db.clone(),
        )
        .await?;

        let user_info = provider_service.get_user(&pat_token).await?;

        // Create a connection for this PAT with the actual username
        let connection = self
            .create_connection(
                provider.id,
                user_id,
                user_info.username, // Use actual GitLab username
                "User".to_string(),
                Some(pat_token), // Store token in connection
                None,            // No refresh token for PAT
                None,            // No installation ID for PAT
                None,            // No metadata
                None,
            )
            .await?;

        // Kick off sync in the background so creating the provider returns
        // quickly. The sync's own guard resets `syncing` on any exit path.
        let manager = Arc::new(self.clone());
        if let Err(e) = manager.spawn_sync_repositories(connection.id).await {
            tracing::warn!(
                "Failed to start background sync for GitLab PAT connection {}: {}",
                connection.id,
                e
            );
            // Don't fail the provider creation if sync can't be kicked off.
        }

        Ok(provider)
    }

    /// Create a git provider entry for a Gitea PAT connection.
    ///
    /// # Arguments
    /// * `name` — display name for the provider
    /// * `pat_token` — Gitea personal access token
    /// * `user_id` — ID of the Temps user creating the connection
    /// * `base_url` — HTTPS base URL of the Gitea instance, e.g.
    ///   `https://git.example.com`. MUST already be validated with
    ///   `validate_git_url` by the calling handler (MUST-FIX 4).
    pub async fn create_gitea_pat_provider(
        &self,
        name: String,
        pat_token: String,
        user_id: i32,
        base_url: String,
    ) -> Result<git_providers::Model, GitProviderManagerError> {
        // MUST-FIX 4: validate the base_url with HTTPS-only validate_git_url,
        // not validate_external_url (which permits plaintext http).
        temps_core::url_validation::validate_git_url(&base_url).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Gitea base_url failed HTTPS validation: {}",
                e
            ))
        })?;

        let auth_method = AuthMethod::PersonalAccessToken {
            token: pat_token.clone(),
        };

        let api_url = format!("{}/api/v1", base_url.trim_end_matches('/'));

        let provider = self
            .create_provider(
                name,
                GitProviderType::Gitea,
                auth_method.clone(),
                Some(base_url.clone()),
                Some(api_url.clone()),
                None,
                false,
            )
            .await?;

        // Resolve the Gitea username via the PAT before creating the connection.
        let provider_service = GitProviderFactory::create_provider(
            GitProviderType::Gitea,
            auth_method,
            Some(base_url),
            Some(api_url),
            self.db.clone(),
        )
        .await?;

        let user_info = provider_service.get_user(&pat_token).await?;

        let connection = self
            .create_connection(
                provider.id,
                user_id,
                user_info.username,
                "User".to_string(),
                Some(pat_token),
                None,
                None,
                None,
                None,
            )
            .await?;

        // Kick off background repository sync.
        let manager = Arc::new(self.clone());
        if let Err(e) = manager.spawn_sync_repositories(connection.id).await {
            tracing::warn!(
                "Failed to start background sync for Gitea PAT connection {}: {}",
                connection.id,
                e
            );
        }

        Ok(provider)
    }

    /// Create a Bitbucket Cloud provider with PAT (Repository/Workspace Access Token)
    /// or App Password authentication.
    ///
    /// Unlike `create_gitea_pat_provider`, there is no user-supplied `base_url` —
    /// Bitbucket Cloud's API base is the fixed constant `https://api.bitbucket.org/2.0`.
    ///
    /// A `bitbucket_webhook_token` is generated at creation time and stored encrypted
    /// in the returned provider model. The UI surfaces the webhook URL:
    /// `{temps_url}/api/webhook/git/bitbucket/events/{token}` for the user to
    /// configure manually in Bitbucket (auto-registration is deferred to v1.5).
    pub async fn create_bitbucket_provider(
        &self,
        name: String,
        auth_method: AuthMethod,
        user_id: i32,
    ) -> Result<git_providers::Model, GitProviderManagerError> {
        let provider = self
            .create_provider(
                name,
                GitProviderType::Bitbucket,
                auth_method.clone(),
                // base_url and api_url are fixed for Bitbucket Cloud.
                Some("https://bitbucket.org".to_string()),
                Some("https://api.bitbucket.org/2.0".to_string()),
                None,
                false,
            )
            .await?;

        // Resolve the Bitbucket user info to populate the connection account name.
        let provider_service = GitProviderFactory::create_provider(
            GitProviderType::Bitbucket,
            auth_method.clone(),
            Some("https://bitbucket.org".to_string()),
            Some("https://api.bitbucket.org/2.0".to_string()),
            self.db.clone(),
        )
        .await?;

        // Use a dummy token string for get_user — BitbucketProvider uses auth_method
        // directly and ignores the access_token parameter.
        let user_info = provider_service.get_user("").await?;

        let access_token = match &auth_method {
            AuthMethod::PersonalAccessToken { token } => Some(token.clone()),
            AuthMethod::BasicAuth { password, .. } => Some(password.clone()),
            _ => None,
        };

        let connection = self
            .create_connection(
                provider.id,
                user_id,
                user_info.username,
                "User".to_string(),
                access_token,
                None,
                None,
                None,
                None,
            )
            .await?;

        // Kick off background repository sync.
        let manager = std::sync::Arc::new(self.clone());
        if let Err(e) = manager.spawn_sync_repositories(connection.id).await {
            tracing::warn!(
                "Failed to start background sync for Bitbucket connection {}: {}",
                connection.id,
                e
            );
        }

        Ok(provider)
    }

    /// Create a Generic/Manual git provider connection.
    ///
    /// Supports two v1 modes:
    /// - **Mode A — Public repository:** `token_opt` is `None` → clones without
    ///   credentials via `git_ops::clone_repo`.
    /// - **Mode B — Private HTTPS token:** `token_opt` is `Some(token)` →
    ///   clones via `git_ops::clone_repo_with_credentials`. The `token_username`
    ///   parameter sets the HTTP Basic username (default `x-access-token`).
    ///
    /// # Security (MUST-FIX 4)
    /// `clone_url` is validated with the HTTPS-only `validate_git_url` at
    /// create time. The clone path re-validates independently (defense-in-depth).
    ///
    /// A `generic_webhook_token` is generated at creation time and stored
    /// encrypted in the returned provider model. The UI surfaces the webhook
    /// URL: `{temps_url}/api/webhook/git/generic/events/{token}` for the user
    /// to configure manually in their git host.
    pub async fn create_generic_provider(
        &self,
        name: String,
        clone_url: String,
        token_username: String,
        token_opt: Option<String>,
        base_url_opt: Option<String>,
        user_id: i32,
    ) -> Result<git_providers::Model, GitProviderManagerError> {
        // MUST-FIX 4: validate clone_url with HTTPS-only validate_git_url
        // at create time.
        temps_core::url_validation::validate_git_url(&clone_url).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Generic provider clone_url failed HTTPS validation: {}",
                e
            ))
        })?;

        let auth_method = match &token_opt {
            Some(token) if !token_username.is_empty() => AuthMethod::BasicAuth {
                username: token_username.clone(),
                password: token.clone(),
            },
            Some(token) => AuthMethod::PersonalAccessToken {
                token: token.clone(),
            },
            None => {
                // Mode A: public repo — use a placeholder OAuth to signal "no creds"
                // The GenericProvider inspects the auth_method and falls through to
                // credential_string() returning None.
                AuthMethod::OAuth {
                    client_id: String::new(),
                    client_secret: String::new(),
                    redirect_uri: String::new(),
                }
            }
        };

        let provider = self
            .create_provider(
                name,
                GitProviderType::Generic,
                auth_method.clone(),
                base_url_opt,
                None, // api_url: Generic has no REST API
                None, // webhook_secret: Generic uses secret-in-path, stored on the project
                false,
            )
            .await?;

        // Generic providers have no user API — use the clone_url host as the
        // account name for display purposes.
        let account_name = reqwest::Url::parse(&clone_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_string()))
            .unwrap_or_else(|| "generic".to_string());

        // Store clone_url and token_username in connection metadata JSON so that
        // the clone path can retrieve them (ADR Decision 1c).
        let connection_metadata = serde_json::json!({
            "clone_url": clone_url,
            "token_username": token_username,
        });

        let access_token = token_opt.clone();

        let _connection = self
            .create_connection(
                provider.id,
                user_id,
                account_name,
                "User".to_string(),
                access_token,
                None,
                None,
                Some(connection_metadata),
                None,
            )
            .await?;

        // Generic providers do not support repository sync — no spawn_sync_repositories call.

        Ok(provider)
    }

    /// Create a git provider entry for GitLab OAuth
    pub async fn create_gitlab_oauth_provider(
        &self,
        name: String,
        client_id: String,
        client_secret: String,
        redirect_uri: String,
        base_url: Option<String>,
    ) -> Result<git_providers::Model, GitProviderManagerError> {
        let auth_method = AuthMethod::OAuth {
            client_id,
            client_secret,
            redirect_uri,
        };

        let provider = self
            .create_provider(
                name,
                GitProviderType::GitLab,
                auth_method,
                base_url
                    .clone()
                    .or_else(|| Some("https://gitlab.com".to_string())), // Base URL for web interface
                base_url
                    .map(|url| format!("{}/api/v4", url))
                    .or_else(|| Some("https://gitlab.com/api/v4".to_string())), // API URL for API calls
                None,
                false,
            )
            .await?;

        Ok(provider)
    }

    // Helper methods

    fn get_auth_method_type(&self, auth_method: &AuthMethod) -> String {
        match auth_method {
            AuthMethod::GitHubApp { .. } => "github_app".to_string(),
            AuthMethod::GitLabApp { .. } => "gitlab_app".to_string(),
            AuthMethod::OAuth { .. } => "oauth".to_string(),
            AuthMethod::PersonalAccessToken { .. } => "pat".to_string(),
            AuthMethod::BasicAuth { .. } => "basic".to_string(),
            AuthMethod::SSHKey { .. } => "ssh".to_string(),
        }
    }

    /// Partial credential update for an existing provider. Each `Option::Some`
    /// replaces the corresponding field; `None` keeps the stored value.
    /// Variant-matching is enforced against the provider's existing auth_method:
    /// you can't turn a PAT provider into a GitHub App here — delete and recreate.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_provider_credentials(
        &self,
        provider_id: i32,
        client_id: Option<String>,
        client_secret: Option<String>,
        app_id: Option<String>,
        app_secret: Option<String>,
        private_key: Option<String>,
        webhook_secret: Option<String>,
        redirect_uri: Option<String>,
        token: Option<String>,
    ) -> Result<git_providers::Model, GitProviderManagerError> {
        let provider = self.get_provider(provider_id).await?;

        // Decrypt current auth config and parse to enum so we can merge per-variant.
        let current_json = self.decrypt_sensitive_data(&provider.auth_config).await?;
        let current_method: AuthMethod = serde_json::from_value(current_json)?;

        let updated_method = match current_method {
            AuthMethod::GitHubApp {
                app_id: existing_app_id,
                client_id: existing_client_id,
                client_secret: existing_client_secret,
                private_key: existing_private_key,
                webhook_secret: existing_webhook_secret,
            } => {
                let new_app_id = match app_id {
                    Some(v) => v.parse::<i32>().map_err(|_| {
                        GitProviderManagerError::InvalidConfiguration(
                            "app_id must be a valid integer for GitHub App".to_string(),
                        )
                    })?,
                    None => existing_app_id,
                };
                AuthMethod::GitHubApp {
                    app_id: new_app_id,
                    client_id: client_id.unwrap_or(existing_client_id),
                    client_secret: client_secret.unwrap_or(existing_client_secret),
                    private_key: private_key.unwrap_or(existing_private_key),
                    webhook_secret: webhook_secret.unwrap_or(existing_webhook_secret),
                }
            }
            AuthMethod::GitLabApp {
                app_id: existing_app_id,
                app_secret: existing_app_secret,
                redirect_uri: existing_redirect_uri,
            } => AuthMethod::GitLabApp {
                app_id: app_id.unwrap_or(existing_app_id),
                app_secret: app_secret.unwrap_or(existing_app_secret),
                redirect_uri: redirect_uri.unwrap_or(existing_redirect_uri),
            },
            AuthMethod::OAuth {
                client_id: existing_client_id,
                client_secret: existing_client_secret,
                redirect_uri: existing_redirect_uri,
            } => AuthMethod::OAuth {
                client_id: client_id.unwrap_or(existing_client_id),
                client_secret: client_secret.unwrap_or(existing_client_secret),
                redirect_uri: redirect_uri.unwrap_or(existing_redirect_uri),
            },
            AuthMethod::PersonalAccessToken {
                token: existing_token,
            } => AuthMethod::PersonalAccessToken {
                token: token.unwrap_or(existing_token),
            },
            AuthMethod::BasicAuth { .. } | AuthMethod::SSHKey { .. } => {
                return Err(GitProviderManagerError::InvalidConfiguration(
                    "Credential editing is not supported for this auth method".to_string(),
                ));
            }
        };

        let auth_config_json = serde_json::to_value(&updated_method)?;
        let encrypted_config = self.encrypt_sensitive_data(&auth_config_json).await?;

        let mut active_model: git_providers::ActiveModel = provider.into();
        active_model.auth_config = Set(encrypted_config);
        active_model.auth_method = Set(self.get_auth_method_type(&updated_method));
        let updated = active_model.update(self.db.as_ref()).await?;

        // Clear cache so new credentials take effect on next provider_service request.
        self.providers_cache.write().await.clear();

        Ok(updated)
    }

    async fn unset_default_providers(&self) -> Result<(), GitProviderManagerError> {
        let default_providers = git_providers::Entity::find()
            .filter(git_providers::Column::IsDefault.eq(true))
            .order_by_desc(git_providers::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        for provider in default_providers {
            let mut active_model: git_providers::ActiveModel = provider.into();
            active_model.is_default = Set(false);
            active_model.update(self.db.as_ref()).await?;
        }

        Ok(())
    }

    /// Deactivate a git provider
    pub async fn deactivate_provider(
        &self,
        provider_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        // Check if provider exists
        let provider = self.get_provider(provider_id).await?;

        if !provider.is_active {
            return Ok(()); // Already deactivated
        }

        // Deactivate the provider
        let mut active_model: git_providers::ActiveModel = provider.into();
        active_model.is_active = Set(false);
        active_model.updated_at = Set(chrono::Utc::now());
        active_model.update(self.db.as_ref()).await?;

        // Remove from cache
        self.providers_cache.write().await.remove(&provider_id);

        Ok(())
    }

    /// Activate a git provider
    pub async fn activate_provider(&self, provider_id: i32) -> Result<(), GitProviderManagerError> {
        // Check if provider exists
        let provider = self.get_provider(provider_id).await?;

        if provider.is_active {
            return Ok(()); // Already activated
        }

        // Activate the provider
        let mut active_model: git_providers::ActiveModel = provider.into();
        active_model.is_active = Set(true);
        active_model.updated_at = Set(chrono::Utc::now());
        active_model.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Permanently delete a git provider (hard delete).
    ///
    /// A provider is only blocked by *projects* that still depend on one of its
    /// connections — never by the mere existence of a connection row. Connections
    /// are listed per-user in the UI, so refusing on connection count made
    /// providers permanently undeletable whenever the connection belonged to
    /// another user, had no owner, or was deactivated: the error named a
    /// connection the caller had no way to find or remove.
    pub async fn delete_provider(&self, provider_id: i32) -> Result<(), GitProviderManagerError> {
        self.delete_provider_safely(provider_id).await
    }

    /// Check if a provider can be safely deleted and return detailed usage information
    pub async fn check_provider_deletion_safety(
        &self,
        provider_id: i32,
    ) -> Result<ProviderDeletionCheck, GitProviderManagerError> {
        // Check if provider exists
        let provider = self.get_provider(provider_id).await?;

        // Get all connections for this provider
        let connections = self.get_all_provider_connections(provider_id).await?;

        if connections.is_empty() {
            return Ok(ProviderDeletionCheck {
                can_delete: true,
                projects_in_use: Vec::new(),
                message: format!(
                    "Provider '{}' can be safely deleted (no connections found)",
                    provider.name
                ),
            });
        }

        let mut projects_in_use = Vec::new();

        // Check each connection for project usage
        for connection in &connections {
            // id/name/slug only — see delete_connection: deserializing full
            // project models turns "used by project X" into an opaque
            // "Database Error: unexpected value for Preset enum" the moment one
            // blocking project has a column value this build can't decode.
            let projects: Vec<(i32, String, String)> = temps_entities::projects::Entity::find()
                .select_only()
                .column(temps_entities::projects::Column::Id)
                .column(temps_entities::projects::Column::Name)
                .column(temps_entities::projects::Column::Slug)
                .filter(
                    temps_entities::projects::Column::GitProviderConnectionId
                        .eq(Some(connection.id)),
                )
                .order_by_desc(temps_entities::projects::Column::CreatedAt)
                .into_tuple()
                .all(self.db.as_ref())
                .await?;

            for (id, name, slug) in projects {
                projects_in_use.push(ProjectUsageInfo {
                    id,
                    name,
                    slug,
                    connection_id: connection.id,
                    connection_name: connection.account_name.clone(),
                });
            }
        }

        if projects_in_use.is_empty() {
            Ok(ProviderDeletionCheck {
                can_delete: true,
                projects_in_use,
                message: format!("Provider '{}' can be safely deleted (connections exist but no projects are using them)", provider.name),
            })
        } else {
            let project_names: Vec<String> = projects_in_use
                .iter()
                .map(|p| format!("'{}' (ID: {})", p.name, p.id))
                .collect();

            Ok(ProviderDeletionCheck {
                can_delete: false,
                projects_in_use: projects_in_use.clone(),
                message: format!(
                    "Cannot delete provider '{}' because it is used by {} project(s): {}",
                    provider.name,
                    projects_in_use.len(),
                    project_names.join(", ")
                ),
            })
        }
    }

    /// Safely delete a git provider only if no projects are using its connections
    pub async fn delete_provider_safely(
        &self,
        provider_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        let deletion_check = self.check_provider_deletion_safety(provider_id).await?;

        if !deletion_check.can_delete {
            return Err(GitProviderManagerError::InvalidConfiguration(
                deletion_check.message,
            ));
        }

        // Get provider info for logging
        let provider = self.get_provider(provider_id).await?;

        // Get all connections to delete them along with the provider
        let connections = self.get_all_provider_connections(provider_id).await?;

        // Delete all repositories associated with these connections
        for connection in &connections {
            repositories::Entity::delete_many()
                .filter(repositories::Column::GitProviderConnectionId.eq(Some(connection.id)))
                .exec(self.db.as_ref())
                .await?;
        }

        // Delete all connections for this provider
        git_provider_connections::Entity::delete_many()
            .filter(git_provider_connections::Column::ProviderId.eq(provider_id))
            .exec(self.db.as_ref())
            .await?;

        // Delete the provider
        git_providers::Entity::delete_by_id(provider_id)
            .exec(self.db.as_ref())
            .await?;

        // Remove from cache
        self.providers_cache.write().await.remove(&provider_id);

        info!(
            "Successfully deleted git provider '{}' (ID: {}) and {} associated connection(s)",
            provider.name,
            provider_id,
            connections.len()
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::git_provider_manager::test_support::*;
    use temps_database::test_utils::TestDatabase;

    /// Regression: a provider whose only connection belongs to another user (or
    /// to nobody, or is deactivated) used to be permanently undeletable — the
    /// UI lists connections per-user, so the "it has 1 connection(s)" error
    /// pointed at a row the caller could not see or remove anywhere. Only
    /// projects actually deploying from the provider may block the delete.
    #[tokio::test]
    async fn delete_provider_removes_connections_the_caller_cannot_see() {
        use chrono::Utc;

        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        let provider = git_providers::ActiveModel {
            name: Set("GitLab".to_string()),
            provider_type: Set("gitlab".to_string()),
            base_url: Set(None),
            api_url: Set(None),
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

        // Two rows the caller can never see: an active but ownerless one
        // (user_id = NULL — invisible to the per-user list yet counted by the
        // old guard) and a deactivated one (invisible to the active-only list
        // but still cascaded on delete).
        let now = Utc::now();
        for (account, active) in [("orphan-account", true), ("stale-account", false)] {
            git_provider_connections::ActiveModel {
                provider_id: Set(provider.id),
                user_id: Set(None),
                account_name: Set(account.to_string()),
                account_type: Set("User".to_string()),
                access_token: Set(None),
                refresh_token: Set(None),
                token_expires_at: Set(None),
                refresh_token_expires_at: Set(None),
                installation_id: Set(None),
                metadata: Set(None),
                is_active: Set(active),
                is_expired: Set(false),
                syncing: Set(false),
                last_synced_at: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
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
            create_test_config_service(db.clone()),
        );

        manager
            .delete_provider(provider.id)
            .await
            .expect("provider with no project usage should delete");

        assert!(
            git_providers::Entity::find_by_id(provider.id)
                .one(db.as_ref())
                .await
                .unwrap()
                .is_none(),
            "provider row should be gone"
        );
        assert!(
            manager
                .get_all_provider_connections(provider.id)
                .await
                .unwrap()
                .is_empty(),
            "the hidden connection should have been cascaded away"
        );
    }
}
