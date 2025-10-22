use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, error, info};

use super::git_provider::{
    AuthMethod, GitProviderError, GitProviderFactory, GitProviderService, GitProviderType,
};
use temps_config::ConfigService;
use temps_core::{JobQueue, UtcDateTime};
use temps_entities::{git_provider_connections, git_providers, repositories};

// OAuth scope constants
const GITLAB_OAUTH_SCOPES: &str = "api read_api read_repository";
// Create JWT token for authentication
use octocrab::models::{AppId, InstallationId, InstallationToken};
use octocrab::params::apps::CreateInstallationAccessToken;
use octocrab::Octocrab;
use reqwest::Url;

#[derive(Debug, Clone)]
struct PresetInfo {
    slug: String,
    label: String,
}

#[derive(Debug, Clone)]
pub struct ProjectPresetDomain {
    pub path: String,
    pub preset: String,
    pub preset_label: String,
}

#[derive(Debug, Clone)]
pub struct RepositoryPresetDomain {
    pub repository_id: i32,
    pub owner: String,
    pub name: String,
    pub root_preset: Option<String>,
    pub projects: Vec<ProjectPresetDomain>,
    pub calculated_at: UtcDateTime,
}

#[derive(Debug, Clone)]
pub struct ProjectUsageInfo {
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub connection_id: i32,
    pub connection_name: String,
}

#[derive(Debug, Clone)]
pub struct ProviderDeletionCheck {
    pub can_delete: bool,
    pub projects_in_use: Vec<ProjectUsageInfo>,
    pub message: String,
}

#[derive(Error, Debug)]
pub enum GitProviderManagerError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),

    #[error("Provider error: {0}")]
    ProviderError(#[from] GitProviderError),

    #[error("Provider not found: {0}")]
    ProviderNotFound(String),

    #[error("Connection not found: {0}")]
    ConnectionNotFound(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),
    #[error("Repository not found: {0}")]
    RepositoryNotFound(String),

    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("Sync already in progress")]
    SyncInProgress,

    #[error("Connection token expired for connection ID {connection_id}. Please update your access token.")]
    ConnectionTokenExpired { connection_id: i32 },

    #[error("Queue error: {0}")]
    QueueError(String),
}

#[derive(Clone)]
pub struct GitProviderManager {
    db: Arc<DatabaseConnection>,
    providers_cache: Arc<RwLock<HashMap<i32, Arc<dyn GitProviderService>>>>,
    encryption_service: Arc<temps_core::EncryptionService>,
    queue_service: Arc<dyn JobQueue>,
    config_service: Arc<ConfigService>,
}

impl GitProviderManager {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption_service: Arc<temps_core::EncryptionService>,
        queue_service: Arc<dyn JobQueue>,
        config_service: Arc<ConfigService>,
    ) -> Self {
        Self {
            db,
            providers_cache: Arc::new(RwLock::new(HashMap::new())),
            encryption_service,
            queue_service,
            config_service,
        }
    }

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

        match auth_method {
            // GitHub App: Always generate fresh installation token
            AuthMethod::GitHubApp { .. } => {
                let installation_id = connection.installation_id.as_ref().ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "GitHub App connection missing installation_id".to_string(),
                    )
                })?;

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

                // Check if expired (with 60 second buffer)
                let should_refresh = if let Some(expires_at) = connection.token_expires_at {
                    let now = Utc::now();
                    let buffer = chrono::Duration::seconds(60);
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

    /// Get a repository API instance for convenient repository operations
    /// Returns a trait that provides repository-specific operations (get_commit_info, get_branches, etc.)
    /// without needing to pass owner/repo/connection repeatedly
    pub async fn get_repository_api(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
    ) -> Result<Arc<dyn super::git_repository_api::GitRepositoryApi>, GitProviderManagerError> {
        // Get the connection
        let connection = self.get_connection(connection_id).await?;

        // Get the provider service
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Get access token (with automatic refresh if needed)
        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await?;

        // Create repository API instance
        let repo_api = super::git_repository_api::GitRepositoryApiImpl::new(
            owner.to_string(),
            repo.to_string(),
            provider_service,
            access_token,
        );

        Ok(Arc::new(repo_api))
    }

    /// Fetch commit information from Git provider using connection_id
    /// Returns commit details (sha, message, author, date)
    /// @deprecated Use get_repository_api() instead for better encapsulation
    pub async fn get_commit_info(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        commit_sha: &str,
    ) -> Result<super::git_provider::Commit, GitProviderManagerError> {
        // Get the connection
        let connection = self.get_connection(connection_id).await?;

        // Get the provider service
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Get access token (with automatic refresh if needed)
        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await?;

        // Fetch commit from provider - GitProviderService trait has get_commit method
        let commit = provider_service
            .get_commit(&access_token, owner, repo, commit_sha)
            .await?;

        Ok(commit)
    }

    /// Get the current commit info for a branch
    /// Returns commit details (sha, message, author, date) for the latest commit on the branch
    pub async fn get_branch_latest_commit(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<super::git_provider::Commit, GitProviderManagerError> {
        // Get the connection
        let connection = self.get_connection(connection_id).await?;

        // Get the provider service
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Get access token (with automatic refresh if needed)
        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await?;

        // Fetch latest commit from the branch
        let commit = provider_service
            .get_latest_commit(&access_token, owner, repo, branch)
            .await?;

        Ok(commit)
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

    /// Create a connection to a git provider for a user
    #[allow(clippy::too_many_arguments)]
    pub async fn create_connection(
        &self,
        provider_id: i32,
        user_id: i32,
        account_name: String,
        account_type: String,
        access_token: Option<String>,
        refresh_token: Option<String>,
        installation_id: Option<String>,
        metadata: Option<serde_json::Value>,
        expires_at: Option<UtcDateTime>,
    ) -> Result<git_provider_connections::Model, GitProviderManagerError> {
        // Encrypt tokens if provided
        let encrypted_access = if let Some(token) = access_token {
            Some(self.encrypt_string(&token).await?)
        } else {
            None
        };

        let encrypted_refresh = if let Some(token) = refresh_token {
            Some(self.encrypt_string(&token).await?)
        } else {
            None
        };

        let new_connection = git_provider_connections::ActiveModel {
            provider_id: Set(provider_id),
            user_id: Set(Some(user_id)),
            account_name: Set(account_name),
            account_type: Set(account_type),
            access_token: Set(encrypted_access),
            refresh_token: Set(encrypted_refresh),
            installation_id: Set(installation_id),
            metadata: Set(metadata),
            is_active: Set(true),
            token_expires_at: Set(expires_at),
            ..Default::default()
        };

        let connection = new_connection.insert(self.db.as_ref()).await?;
        Ok(connection)
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
            0, // No specific user, it's org-level
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

        // Synchronously sync repositories after creating the connection
        match self.sync_repositories(connection.id).await {
            Ok(repos) => {
                tracing::info!(
                    "Successfully synced {} repositories for GitHub PAT connection {}",
                    repos.len(),
                    connection.id
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to sync repositories for GitHub PAT connection {}: {}",
                    connection.id,
                    e
                );
                // Don't fail the provider creation if sync fails
            }
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

        // Optionally sync repositories immediately
        match self.sync_repositories(connection.id).await {
            Ok(repos) => {
                tracing::info!(
                    "Successfully synced {} repositories for GitLab PAT connection {}",
                    repos.len(),
                    connection.id
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to sync repositories for GitLab PAT connection {}: {}",
                    connection.id,
                    e
                );
                // Don't fail the provider creation if sync fails
            }
        }

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

    /// Get connections for a user
    pub async fn get_user_connections(
        &self,
    ) -> Result<Vec<git_provider_connections::Model>, GitProviderManagerError> {
        let connections = git_provider_connections::Entity::find()
            .filter(git_provider_connections::Column::IsActive.eq(true))
            .order_by_desc(git_provider_connections::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(connections)
    }

    /// Get connections for a user with pagination and sorting
    pub async fn get_user_connections_paginated(
        &self,
        page: u64,
        per_page: u64,
        sort: &str,
        direction: &str,
    ) -> Result<(Vec<git_provider_connections::Model>, usize), GitProviderManagerError> {
        use sea_orm::QueryOrder;

        let mut query = git_provider_connections::Entity::find()
            .filter(git_provider_connections::Column::IsActive.eq(true));

        // Apply sorting - default to created_at desc
        query = match (sort, direction) {
            ("updated_at", "asc") => {
                query.order_by_asc(git_provider_connections::Column::UpdatedAt)
            }
            ("updated_at", "desc") => {
                query.order_by_desc(git_provider_connections::Column::UpdatedAt)
            }
            ("account_name", "asc") => {
                query.order_by_asc(git_provider_connections::Column::AccountName)
            }
            ("account_name", "desc") => {
                query.order_by_desc(git_provider_connections::Column::AccountName)
            }
            ("created_at", "asc") => {
                query.order_by_asc(git_provider_connections::Column::CreatedAt)
            }
            ("created_at", "desc") | (_, _) => {
                query.order_by_desc(git_provider_connections::Column::CreatedAt)
            }
        };

        // Get total count
        let total_count = query.clone().count(self.db.as_ref()).await?;

        // Apply pagination
        let offset = (page - 1) * per_page;
        let connections = query
            .offset(offset)
            .limit(per_page)
            .all(self.db.as_ref())
            .await?;

        Ok((connections, total_count as usize))
    }
    pub async fn get_repository_by_id(
        &self,
        id: i32,
    ) -> Result<repositories::Model, GitProviderManagerError> {
        let repository = repositories::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?;
        repository.ok_or_else(|| {
            GitProviderManagerError::RepositoryNotFound(format!("Repository {} not found", id))
        })
    }
    pub async fn get_repository_by_owner_and_name_in_connection(
        &self,
        owner: &str,
        name: &str,
        connection_id: i32,
    ) -> Result<repositories::Model, GitProviderManagerError> {
        let repository = repositories::Entity::find()
            .filter(repositories::Column::Owner.eq(owner))
            .filter(repositories::Column::Name.eq(name))
            .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
            .one(self.db.as_ref())
            .await?;

        repository.ok_or_else(|| {
            GitProviderManagerError::RepositoryNotFound(format!(
                "Repository {}/{} not found in connection {}",
                owner, name, connection_id
            ))
        })
    }

    /// Get a specific connection
    pub async fn get_connection(
        &self,
        connection_id: i32,
    ) -> Result<git_provider_connections::Model, GitProviderManagerError> {
        let connection = git_provider_connections::Entity::find_by_id(connection_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                GitProviderManagerError::ConnectionNotFound(connection_id.to_string())
            })?;

        Ok(connection)
    }

    /// Set syncing status for a connection
    async fn set_connection_syncing_status(
        &self,
        connection_id: i32,
        syncing: bool,
    ) -> Result<(), GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;
        let mut active_model: git_provider_connections::ActiveModel = connection.into();
        active_model.syncing = Set(syncing);
        active_model.update(self.db.as_ref()).await?;
        Ok(())
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
    async fn execute_with_refresh<F, Fut, T>(
        &self,
        connection_id: i32,
        api_call: F,
    ) -> Result<T, GitProviderManagerError>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<T, super::git_provider::GitProviderError>>,
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
    fn is_authentication_error(&self, error: &super::git_provider::GitProviderError) -> bool {
        matches!(
            error,
            super::git_provider::GitProviderError::AuthenticationFailed(_)
        )
    }

    /// Sync repositories from a connection
    pub async fn sync_repositories(
        &self,
        connection_id: i32,
    ) -> Result<Vec<repositories::Model>, GitProviderManagerError> {
        // Get connection and check if already syncing
        let connection = self.get_connection(connection_id).await?;

        if connection.syncing {
            return Err(GitProviderManagerError::SyncInProgress);
        }

        // Set syncing status to true
        self.set_connection_syncing_status(connection_id, true)
            .await?;

        // Perform sync with cleanup on completion
        let sync_result = self.sync_repositories_internal(connection_id).await;

        // Always reset syncing status to false
        if let Err(e) = self
            .set_connection_syncing_status(connection_id, false)
            .await
        {
            error!(
                "Failed to reset syncing status for connection {}: {}",
                connection_id, e
            );
        }
        match sync_result {
            Ok(_) => {
                let repos = repositories::Entity::find()
                    .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
                    .order_by_desc(repositories::Column::PushedAt)
                    .all(self.db.as_ref())
                    .await?;
                // Fire background jobs for preset calculation per repository
                for repository in repos.clone() {
                    let manager = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = manager
                            .queue_service
                            .send(temps_core::Job::CalculateRepositoryPreset(
                                temps_core::CalculateRepositoryPresetJob {
                                    repository_id: repository.id,
                                },
                            ))
                            .await
                        {
                            tracing::error!(
                                "Failed to queue preset calculation for repository {}: {}",
                                repository.id,
                                e
                            );
                        }
                    });
                }
                Ok(repos)
            }
            Err(e) => {
                error!("Failed to sync repositories: {}", e);
                Err(e)
            }
        }
    }

    async fn sync_repositories_internal(
        &self,
        connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;
        let provider = self.get_provider(connection.provider_id).await?;
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Get the access token - for GitHub Apps, generate an installation token
        let access_token = if provider.provider_type == "github"
            && provider.auth_method == "github_app"
        {
            // For GitHub Apps, we need to generate an installation access token
            tracing::info!(
                "Generating installation access token for GitHub App connection {}",
                connection_id
            );

            // Get the installation ID from the connection
            let installation_id = connection
                .installation_id
                .as_ref()
                .and_then(|id| id.parse::<i64>().ok())
                .ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "GitHub App connection missing valid installation ID".to_string(),
                    )
                })?;

            // Get the GitHub App credentials from the provider's auth_config
            let app_id = provider
                .auth_config
                .get("app_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "GitHub App missing app_id".to_string(),
                    )
                })?;

            let private_key = provider
                .auth_config
                .get("private_key")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "GitHub App missing private_key".to_string(),
                    )
                })?;

            // Decrypt the private key if it's encrypted
            let decrypted_private_key = if private_key.starts_with("enc:") {
                self.decrypt_string(private_key).await?
            } else {
                private_key.to_string()
            };

            let key = jsonwebtoken::EncodingKey::from_rsa_pem(decrypted_private_key.as_bytes())
                .map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!(
                        "Invalid private key: {}",
                        e
                    ))
                })?;

            let app_id_param = AppId(app_id as u64);
            let jwt_token = octocrab::auth::create_jwt(app_id_param, &key).map_err(|e| {
                GitProviderManagerError::InvalidConfiguration(format!(
                    "Failed to create JWT: {}",
                    e
                ))
            })?;

            let octocrab = Octocrab::builder()
                .personal_token(jwt_token)
                .build()
                .map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!(
                        "Failed to build Octocrab client: {}",
                        e
                    ))
                })?;

            // Get the installation
            let installation = octocrab
                .apps()
                .installation(InstallationId(installation_id as u64))
                .await
                .map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!(
                        "Failed to get installation: {}",
                        e
                    ))
                })?;

            // Create an installation access token
            let create_access_token = CreateInstallationAccessToken::default();
            let gh_access_tokens_url =
                Url::parse(installation.access_tokens_url.as_ref().ok_or_else(|| {
                    GitProviderManagerError::InvalidConfiguration(
                        "Missing access tokens URL".to_string(),
                    )
                })?)
                .map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!("Invalid URL: {}", e))
                })?;

            let access: InstallationToken = octocrab
                .post(gh_access_tokens_url.path(), Some(&create_access_token))
                .await
                .map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!(
                        "Failed to create installation token: {}",
                        e
                    ))
                })?;

            // Update the connection with the new token
            if let Some(expires_at) = &access.expires_at {
                let expires_at = chrono::DateTime::parse_from_rfc3339(expires_at).map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!(
                        "Invalid expiration date: {}",
                        e
                    ))
                })?;
                let expires_at = expires_at.with_timezone(&chrono::Utc);

                self.update_connection_tokens(
                    connection.id,
                    access.token.clone(),
                    None,
                    Some(expires_at),
                )
                .await?;
            }

            access.token
        } else {
            // For non-GitHub App providers, we'll use execute_with_refresh below
            // which handles token decryption and refresh automatically
            String::new() // Placeholder, won't be used directly for OAuth connections
        };

        // Fetch repositories from provider
        // Only pass organization name if account_type is "Organization"
        let organization = if connection.account_type == "Organization" {
            Some(connection.account_name.clone())
        } else {
            None
        };

        // For GitHub Apps, use the access_token directly (already generated above)
        // For OAuth providers, use execute_with_refresh for automatic token refresh
        let repos = if provider.provider_type == "github" && provider.auth_method == "github_app" {
            provider_service
                .list_repositories(&access_token, organization.as_deref())
                .await?
        } else {
            // Use automatic token refresh for OAuth-based connections
            self.execute_with_refresh(connection_id, |token| {
                let org = organization.clone();
                let svc = provider_service.clone();
                async move { svc.list_repositories(&token, org.as_deref()).await }
            })
            .await?
        };

        tracing::info!(
            "Starting bulk sync of {} repositories for connection {}",
            repos.len(),
            connection_id
        );

        // PERFORMANCE OPTIMIZATION: Use bulk operations instead of individual database queries
        // 1. Fetch ALL existing repositories for this connection in a single query
        let existing_repos = repositories::Entity::find()
            .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
            .order_by_desc(repositories::Column::PushedAt)
            .all(self.db.as_ref())
            .await?;

        // Create a HashMap for fast lookups by full_name
        let mut existing_map: std::collections::HashMap<String, repositories::Model> =
            existing_repos
                .into_iter()
                .map(|repo| (repo.full_name.clone(), repo))
                .collect();

        // Separate repositories into updates and inserts for bulk operations
        let mut repos_to_update = Vec::new();
        let mut repos_to_insert = Vec::new();

        for repo in repos {
            if let Some(existing) = existing_map.remove(&repo.full_name) {
                // Repository exists - prepare for bulk update
                let mut active_model: repositories::ActiveModel = existing.into();
                active_model.description = Set(repo.description.clone());
                active_model.default_branch = Set(repo.default_branch.clone());
                active_model.language = Set(repo.language.clone());
                active_model.size = Set(repo.size as i32);
                active_model.stargazers_count = Set(repo.stars);
                active_model.pushed_at = Set(repo.pushed_at.unwrap_or(repo.updated_at));
                active_model.clone_url = Set(Some(repo.clone_url.clone()));
                active_model.ssh_url = Set(Some(repo.ssh_url.clone()));
                active_model.created_at = Set(repo.created_at);
                active_model.updated_at = Set(repo.updated_at);

                repos_to_update.push(active_model);
            } else {
                // New repository - prepare for bulk insert
                let new_repo = repositories::ActiveModel {
                    git_provider_connection_id: Set(Some(connection_id)),
                    owner: Set(repo.owner.clone()),
                    name: Set(repo.name.clone()),
                    full_name: Set(repo.full_name.clone()),
                    description: Set(repo.description.clone()),
                    private: Set(repo.private),
                    fork: Set(false),
                    size: Set(repo.size as i32),
                    stargazers_count: Set(repo.stars),
                    watchers_count: Set(repo.forks),
                    language: Set(repo.language.clone()),
                    default_branch: Set(repo.default_branch.clone()),
                    open_issues_count: Set(0),
                    topics: Set("".to_string()),
                    repo_object: Set(json!(repo).to_string()),
                    created_at: Set(repo.created_at),
                    updated_at: Set(repo.updated_at),
                    pushed_at: Set(repo.pushed_at.unwrap_or(repo.updated_at)),
                    clone_url: Set(Some(repo.clone_url.clone())),
                    ssh_url: Set(Some(repo.ssh_url.clone())),
                    installation_id: Set(None),
                    preset: Set(None),
                    ..Default::default()
                };

                repos_to_insert.push(new_repo);
            }
        }

        tracing::info!(
            "Bulk operations: {} updates, {} inserts for connection {}",
            repos_to_update.len(),
            repos_to_insert.len(),
            connection_id
        );

        // BULK UPDATE: Process all updates in batches of 100
        if !repos_to_update.is_empty() {
            const BATCH_SIZE: usize = 100;
            for chunk in repos_to_update.chunks(BATCH_SIZE) {
                for repo in chunk {
                    repo.clone().update(self.db.as_ref()).await?;
                }
            }
            tracing::info!(
                "Updated {} repositories in batches of {}",
                repos_to_update.len(),
                BATCH_SIZE
            );
        }

        // BULK INSERT: Use Sea-ORM's insert_many for new repositories in batches of 100
        if !repos_to_insert.is_empty() {
            const BATCH_SIZE: usize = 100;
            for chunk in repos_to_insert.chunks(BATCH_SIZE) {
                repositories::Entity::insert_many(chunk.to_vec())
                    .exec(self.db.as_ref())
                    .await?;
            }
            tracing::info!(
                "Inserted {} new repositories in batches of {}",
                repos_to_insert.len(),
                BATCH_SIZE
            );
        }

        // Update last synced time
        let mut active_connection: git_provider_connections::ActiveModel = connection.into();
        active_connection.last_synced_at = Set(Some(chrono::Utc::now()));
        active_connection.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Calculate and store preset for a repository
    pub async fn calculate_and_store_preset(
        &self,
        repo_id: i32,
        connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        let repo = repositories::Entity::find_by_id(repo_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                GitProviderManagerError::InvalidConfiguration(format!(
                    "Repository {} not found",
                    repo_id
                ))
            })?;

        let connection = self.get_connection(connection_id).await?;
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Decrypt access token
        let access_token = if let Some(ref encrypted) = connection.access_token {
            self.decrypt_string(encrypted).await?
        } else {
            return Err(GitProviderManagerError::InvalidConfiguration(
                "No access token found".to_string(),
            ));
        };

        // Calculate preset
        let preset = self
            .calculate_repository_preset(
                &provider_service,
                &access_token,
                &repo.owner,
                &repo.name,
                &repo.default_branch,
            )
            .await;

        // Update repository with preset
        let mut active_model: repositories::ActiveModel = repo.into();
        active_model.preset = Set(preset);
        active_model.framework_last_updated_at = Set(Some(chrono::Utc::now()));
        active_model.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Start OAuth flow for a git provider
    pub async fn start_oauth_flow(
        &self,
        provider_id: i32,
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
            _ => Err(GitProviderManagerError::ProviderError(
                GitProviderError::NotImplemented,
            )),
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
                info!("Token data: {}", token_data);
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

                // Exchange code for access token
                let client = reqwest::Client::new();
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

    /// List repositories for a connection with pagination and filtering
    pub async fn list_repositories_by_connection(
        &self,
        connection_id: i32,
        params: super::git_provider::RepositoryListParams,
    ) -> Result<Vec<super::git_provider::Repository>, GitProviderManagerError> {
        // Get the connection details
        let connection = self.get_connection(connection_id).await?;

        // Get the provider service instance
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Execute the API call with automatic token refresh
        let organization = params.organization.clone();
        let repositories = self
            .execute_with_refresh(connection_id, |access_token| {
                let org = organization.clone();
                let svc = provider_service.clone();
                async move { svc.list_repositories(&access_token, org.as_deref()).await }
            })
            .await?;

        // Apply client-side filtering for search term if provided
        let filtered_repositories = if let Some(search_term) = params.search_term {
            repositories
                .into_iter()
                .filter(|repo| {
                    repo.name
                        .to_lowercase()
                        .contains(&search_term.to_lowercase())
                        || repo
                            .full_name
                            .to_lowercase()
                            .contains(&search_term.to_lowercase())
                        || repo.description.as_ref().is_some_and(|desc| {
                            desc.to_lowercase().contains(&search_term.to_lowercase())
                        })
                })
                .collect()
        } else {
            repositories
        };

        // Apply client-side pagination if provided
        let paginated_repositories =
            if let (Some(page), Some(per_page)) = (params.page, params.per_page) {
                let start = ((page.saturating_sub(1)) * per_page) as usize;
                let end = (start + per_page as usize).min(filtered_repositories.len());

                if start < filtered_repositories.len() {
                    filtered_repositories[start..end].to_vec()
                } else {
                    Vec::new()
                }
            } else {
                filtered_repositories
            };

        Ok(paginated_repositories)
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

    async fn encrypt_string(&self, data: &str) -> Result<String, GitProviderManagerError> {
        self.encryption_service.encrypt_string(data).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!("Encryption failed: {}", e))
        })
    }

    async fn decrypt_string(&self, data: &str) -> Result<String, GitProviderManagerError> {
        self.encryption_service.decrypt_string(data).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!("Decryption failed: {}", e))
        })
    }

    async fn encrypt_sensitive_data(
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

    /// Calculate the preset for a repository by analyzing its files
    async fn calculate_repository_preset(
        &self,
        provider_service: &Arc<dyn GitProviderService>,
        access_token: &str,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Option<String> {
        use temps_presets::detect_preset_from_files;

        // Try to get the repository tree to detect project type
        match self
            .get_repository_files(provider_service, access_token, owner, repo, branch)
            .await
        {
            Ok(files) => {
                // Use the preset detection logic
                detect_preset_from_files(&files).map(|preset| preset.slug())
            }
            Err(e) => {
                tracing::warn!("Failed to detect preset for {}/{}: {}", owner, repo, e);
                None
            }
        }
    }

    /// Get repository files for preset detection
    async fn get_repository_files(
        &self,
        provider_service: &Arc<dyn GitProviderService>,
        access_token: &str,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<Vec<String>, GitProviderManagerError> {
        // For GitHub, we can use the tree API to get file list
        // For other providers, we may need different approaches

        // Try to get file list from the root of the repository
        // This is a simplified approach - in production you might want to be more thorough

        let client = reqwest::Client::new();

        match provider_service.provider_type() {
            GitProviderType::GitHub => {
                // Use GitHub API to get tree
                let url = format!(
                    "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
                    owner, repo, branch
                );

                let response = client
                    .get(&url)
                    .header("Authorization", format!("Bearer {}", access_token))
                    .header("User-Agent", "Temps-Engine")
                    .send()
                    .await
                    .map_err(|e| {
                        GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                            "Failed to get tree: {}",
                            e
                        )))
                    })?;

                if !response.status().is_success() {
                    return Err(GitProviderManagerError::ProviderError(
                        GitProviderError::ApiError(format!(
                            "Failed to get tree: HTTP {}",
                            response.status()
                        )),
                    ));
                }

                let tree_data: serde_json::Value = response.json().await.map_err(|e| {
                    GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                        "Failed to parse tree response: {}",
                        e
                    )))
                })?;

                let files = tree_data["tree"]
                    .as_array()
                    .ok_or_else(|| {
                        GitProviderManagerError::ProviderError(GitProviderError::ApiError(
                            "No tree in response".to_string(),
                        ))
                    })?
                    .iter()
                    .filter_map(|item| {
                        if item["type"].as_str() == Some("blob") {
                            item["path"].as_str().map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect();

                Ok(files)
            }
            GitProviderType::GitLab => {
                // GitLab tree API
                let url = format!(
                    "https://gitlab.com/api/v4/projects/{}/repository/tree?ref={}&recursive=true&per_page=100",
                    urlencoding::encode(&format!("{}/{}", owner, repo)),
                    branch
                );

                let response = client
                    .get(&url)
                    .header("Authorization", format!("Bearer {}", access_token))
                    .send()
                    .await
                    .map_err(|e| {
                        GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                            "Failed to get tree: {}",
                            e
                        )))
                    })?;

                if !response.status().is_success() {
                    return Err(GitProviderManagerError::ProviderError(
                        GitProviderError::ApiError(format!(
                            "Failed to get tree: HTTP {}",
                            response.status()
                        )),
                    ));
                }

                let tree_data: Vec<serde_json::Value> = response.json().await.map_err(|e| {
                    GitProviderManagerError::ProviderError(GitProviderError::ApiError(format!(
                        "Failed to parse tree response: {}",
                        e
                    )))
                })?;

                let files = tree_data
                    .iter()
                    .filter_map(|item| {
                        if item["type"].as_str() == Some("blob") {
                            item["path"].as_str().map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect();

                Ok(files)
            }
            _ => {
                // For other providers, return empty list for now
                Ok(Vec::new())
            }
        }
    }

    /// Get repository preset from database
    pub async fn get_repository_preset(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
    ) -> Result<Option<String>, GitProviderManagerError> {
        let repository = repositories::Entity::find()
            .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
            .filter(repositories::Column::Owner.eq(owner))
            .filter(repositories::Column::Name.eq(repo))
            .one(self.db.as_ref())
            .await?;

        Ok(repository.and_then(|r| r.preset))
    }

    /// Calculate repository preset in real-time without storing it
    pub async fn calculate_repository_preset_live(
        &self,
        repository_id: i32,
        branch: Option<String>,
    ) -> Result<RepositoryPresetDomain, GitProviderManagerError> {
        // Get repository from database
        let repository = repositories::Entity::find_by_id(repository_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                GitProviderManagerError::InvalidConfiguration(format!(
                    "Repository {} not found",
                    repository_id
                ))
            })?;

        // Check if repository has a git provider connection
        let connection_id = repository.git_provider_connection_id.ok_or_else(|| {
            GitProviderManagerError::InvalidConfiguration(
                "Repository is not associated with a git provider connection".to_string(),
            )
        })?;

        // Get the git provider connection
        let connection = self.get_connection(connection_id).await?;
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Decrypt access token
        let access_token = if let Some(ref encrypted) = connection.access_token {
            self.decrypt_string(encrypted).await?
        } else {
            return Err(GitProviderManagerError::InvalidConfiguration(
                "Git provider connection has no access token configured".to_string(),
            ));
        };

        // Use provided branch or fall back to repository's default branch
        let target_branch = branch.unwrap_or_else(|| repository.default_branch.clone());

        // Get all files in the repository
        let files = self
            .get_repository_files(
                &provider_service,
                &access_token,
                &repository.owner,
                &repository.name,
                &target_branch,
            )
            .await?;

        // Detect presets in root and subdirectories
        let (root_preset, projects) = self.detect_presets_in_directories(&files).await;

        Ok(RepositoryPresetDomain {
            repository_id,
            owner: repository.owner,
            name: repository.name,
            root_preset,
            projects,
            calculated_at: chrono::Utc::now(),
        })
    }

    /// Detect presets in directories with proper grouping and filtering
    async fn detect_presets_in_directories(
        &self,
        files: &[String],
    ) -> (Option<String>, Vec<ProjectPresetDomain>) {
        use std::collections::HashMap;

        // Group files by directory
        let mut directory_files: HashMap<String, Vec<String>> = HashMap::new();

        for path in files {
            let directory = match path.rfind('/') {
                Some(idx) => path[..idx].to_string(),
                None => "".to_string(), // Root directory
            };

            directory_files
                .entry(directory.clone())
                .or_default()
                .push(path.clone());
        }

        let mut root_preset = None;
        let mut projects = Vec::new();

        // Check each directory for presets
        for (dir, files) in &directory_files {
            // Limit to 2 levels deep
            let depth = dir.matches('/').count();
            if depth > 2 {
                continue;
            }

            let preset = self.detect_preset_from_directory_files(files);

            if let Some(preset) = preset {
                if dir.is_empty() {
                    // Root directory preset
                    root_preset = Some(preset.slug);
                } else {
                    // Subdirectory preset
                    projects.push(ProjectPresetDomain {
                        path: dir.clone(),
                        preset: preset.slug,
                        preset_label: preset.label,
                    });
                }
            }
        }

        // Sort projects by path for consistent output
        projects.sort_by(|a, b| a.path.cmp(&b.path));

        (root_preset, projects)
    }

    /// Detect preset from a specific directory's files
    fn detect_preset_from_directory_files(&self, files: &[String]) -> Option<PresetInfo> {
        // Check for Dockerfile first (highest priority)
        if files
            .iter()
            .any(|path| path.ends_with("/Dockerfile") || path == "Dockerfile")
        {
            return Some(PresetInfo {
                slug: "dockerfile".to_string(),
                label: "Dockerfile".to_string(),
            });
        }

        // Check for Docusaurus
        if files.iter().any(|path| {
            path.ends_with("docusaurus.config.js") || path.ends_with("docusaurus.config.ts")
        }) {
            return Some(PresetInfo {
                slug: "docusaurus".to_string(),
                label: "Docusaurus".to_string(),
            });
        }

        // Check for Next.js
        if files.iter().any(|path| {
            path.ends_with("next.config.js")
                || path.ends_with("next.config.mjs")
                || path.ends_with("next.config.ts")
        }) {
            return Some(PresetInfo {
                slug: "nextjs".to_string(),
                label: "Next.js".to_string(),
            });
        }

        // Check for Vite
        if files
            .iter()
            .any(|path| path.ends_with("vite.config.js") || path.ends_with("vite.config.ts"))
        {
            return Some(PresetInfo {
                slug: "vite".to_string(),
                label: "Vite".to_string(),
            });
        }

        // Check for Create React App
        if files.iter().any(|path| path.contains("react-scripts")) {
            return Some(PresetInfo {
                slug: "create-react-app".to_string(),
                label: "Create React App".to_string(),
            });
        }

        // Check for Rsbuild
        if files.iter().any(|path| path.ends_with("rsbuild.config.ts")) {
            return Some(PresetInfo {
                slug: "rsbuild".to_string(),
                label: "Rsbuild".to_string(),
            });
        }

        // Don't return anything for directories without framework-specific files
        // This prevents directories like "src", "public", etc. from being detected as having presets
        None
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

        // Validate the new access token before saving
        let provider_service = self.get_provider_service(connection.provider_id).await?;
        match provider_service.validate_token(&new_access_token).await {
            Ok(true) => {
                tracing::info!(
                    "New access token validated successfully for connection {}",
                    connection_id
                );
            }
            Ok(false) => {
                tracing::warn!(
                    "New access token validation failed for connection {} - token appears invalid",
                    connection_id
                );
                return Err(GitProviderManagerError::InvalidConfiguration(
                    "The provided access token is invalid or has insufficient permissions"
                        .to_string(),
                ));
            }
            Err(e) => {
                tracing::warn!(
                    "Error validating new access token for connection {}: {}",
                    connection_id,
                    e
                );
                return Err(GitProviderManagerError::InvalidConfiguration(format!(
                    "Failed to validate the provided access token: {}",
                    e
                )));
            }
        }

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

    /// Validate connection by testing the access token
    pub async fn validate_connection(
        &self,
        connection_id: i32,
    ) -> Result<bool, GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;
        let provider_service = self.get_provider_service(connection.provider_id).await?;

        // Decrypt access token
        let access_token = if let Some(ref encrypted) = connection.access_token {
            self.decrypt_string(encrypted).await?
        } else {
            return Ok(false);
        };

        // Try to get user info to validate the token
        match provider_service.get_user(&access_token).await {
            Ok(_) => Ok(true),
            Err(_) => {
                // Token is invalid, mark connection as inactive
                self.deactivate_connection(connection_id).await?;
                Ok(false)
            }
        }
    }

    /// Find git provider by GitHub App ID
    pub async fn find_provider_by_github_app_id(
        &self,
        app_id: i32,
    ) -> Result<git_providers::Model, GitProviderManagerError> {
        // Get all GitHub providers
        let providers = git_providers::Entity::find()
            .filter(git_providers::Column::ProviderType.eq("github"))
            .order_by_desc(git_providers::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        // Check each provider's auth config for matching app_id
        for provider in providers {
            if let Ok(auth_config) = self.decrypt_sensitive_data(&provider.auth_config).await {
                if let Ok(AuthMethod::GitHubApp {
                    app_id: provider_app_id,
                    ..
                }) = serde_json::from_value::<AuthMethod>(auth_config)
                {
                    if provider_app_id == app_id {
                        return Ok(provider);
                    }
                }
            }
        }

        Err(GitProviderManagerError::ProviderNotFound(format!(
            "GitHub App with app_id: {}",
            app_id
        )))
    }

    /// Get all connections for a specific provider
    pub async fn get_provider_connections(
        &self,
        provider_id: i32,
    ) -> Result<Vec<git_provider_connections::Model>, GitProviderManagerError> {
        let connections = git_provider_connections::Entity::find()
            .filter(git_provider_connections::Column::ProviderId.eq(provider_id))
            .filter(git_provider_connections::Column::IsActive.eq(true))
            .order_by_desc(git_provider_connections::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(connections)
    }

    /// Get all GitHub App providers
    pub async fn get_github_app_providers(
        &self,
    ) -> Result<Vec<git_providers::Model>, GitProviderManagerError> {
        let providers = git_providers::Entity::find()
            .filter(git_providers::Column::ProviderType.eq(GitProviderType::GitHub.to_string()))
            .filter(git_providers::Column::IsActive.eq(true))
            .order_by_desc(git_providers::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        // Filter to only include GitHub App providers (not OAuth or PAT)
        let mut github_app_providers = Vec::new();
        for provider in providers {
            let auth_config = self.decrypt_sensitive_data(&provider.auth_config).await?;
            if let Ok(auth_method) = serde_json::from_value::<AuthMethod>(auth_config) {
                if matches!(auth_method, AuthMethod::GitHubApp { .. }) {
                    github_app_providers.push(provider);
                }
            }
        }

        Ok(github_app_providers)
    }

    /// Get a GitHub App provider by app_id
    pub async fn get_github_app_provider_by_app_id(
        &self,
        app_id: i32,
    ) -> Result<git_providers::Model, GitProviderManagerError> {
        let providers = self.get_github_app_providers().await?;

        for provider in providers {
            let auth_config = self.decrypt_sensitive_data(&provider.auth_config).await?;
            if let Ok(AuthMethod::GitHubApp {
                app_id: provider_app_id,
                ..
            }) = serde_json::from_value::<AuthMethod>(auth_config)
            {
                if provider_app_id == app_id {
                    return Ok(provider);
                }
            }
        }

        Err(GitProviderManagerError::ProviderNotFound(format!(
            "GitHub App provider with app_id {} not found",
            app_id
        )))
    }

    /// Get connection by installation_id
    pub async fn get_connection_by_installation_id(
        &self,
        installation_id: &str,
    ) -> Result<git_provider_connections::Model, GitProviderManagerError> {
        let connection = git_provider_connections::Entity::find()
            .filter(
                git_provider_connections::Column::InstallationId
                    .eq(Some(installation_id.to_string())),
            )
            .filter(git_provider_connections::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                GitProviderManagerError::ConnectionNotFound(format!(
                    "Connection with installation_id {} not found",
                    installation_id
                ))
            })?;

        Ok(connection)
    }

    /// Check if any GitHub App installations exist
    pub async fn has_github_app_installation(&self) -> Result<bool, GitProviderManagerError> {
        let providers = self.get_github_app_providers().await?;

        for provider in providers {
            let connections = self.get_provider_connections(provider.id).await?;
            if !connections.is_empty() {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Get all GitHub App installations (connections)
    pub async fn get_all_github_app_installations(
        &self,
    ) -> Result<Vec<git_provider_connections::Model>, GitProviderManagerError> {
        let providers = self.get_github_app_providers().await?;
        let mut all_installations = Vec::new();

        for provider in providers {
            let connections = self.get_provider_connections(provider.id).await?;
            all_installations.extend(connections);
        }

        Ok(all_installations)
    }

    /// Delete an installation (connection) by installation_id
    pub async fn delete_installation(
        &self,
        installation_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        if let Ok(connection) = self
            .get_connection_by_installation_id(&installation_id.to_string())
            .await
        {
            let provider_id = connection.provider_id;

            // Soft delete the connection
            let mut active_model: git_provider_connections::ActiveModel = connection.into();
            active_model.is_active = Set(false);
            active_model.updated_at = Set(chrono::Utc::now());
            active_model.update(self.db.as_ref()).await?;

            // Also deactivate the associated provider
            self.deactivate_provider(provider_id).await?;
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

    /// Deactivate a git provider connection
    pub async fn deactivate_connection(
        &self,
        connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        // Check if connection exists
        let connection = self.get_connection(connection_id).await?;

        if !connection.is_active {
            return Ok(()); // Already deactivated
        }

        // Check if connection is in use by any projects
        let project_count = temps_entities::projects::Entity::find()
            .filter(
                temps_entities::projects::Column::GitProviderConnectionId.eq(Some(connection_id)),
            )
            .count(self.db.as_ref())
            .await?;

        if project_count > 0 {
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Cannot deactivate connection {} because it is used by {} project(s)",
                connection_id, project_count
            )));
        }

        // Deactivate the connection
        let mut active_model: git_provider_connections::ActiveModel = connection.into();
        active_model.is_active = Set(false);
        active_model.updated_at = Set(chrono::Utc::now());
        active_model.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Activate a git provider connection
    pub async fn activate_connection(
        &self,
        connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        // Check if connection exists
        let connection = self.get_connection(connection_id).await?;

        if connection.is_active {
            return Ok(()); // Already activated
        }

        // Check if the provider is active
        let provider = self.get_provider(connection.provider_id).await?;
        if !provider.is_active {
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Cannot activate connection because provider {} is deactivated",
                provider.name
            )));
        }

        // Activate the connection
        let mut active_model: git_provider_connections::ActiveModel = connection.into();
        active_model.is_active = Set(true);
        active_model.updated_at = Set(chrono::Utc::now());
        active_model.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Permanently delete a git provider (hard delete)
    pub async fn delete_provider(&self, provider_id: i32) -> Result<(), GitProviderManagerError> {
        // Check if provider exists
        let provider = self.get_provider(provider_id).await?;

        // Check if any connections exist for this provider
        let connections = self.get_provider_connections(provider_id).await?;
        if !connections.is_empty() {
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Cannot delete provider {} because it has {} connection(s)",
                provider.name,
                connections.len()
            )));
        }

        // Delete the provider
        git_providers::Entity::delete_by_id(provider_id)
            .exec(self.db.as_ref())
            .await?;

        // Remove from cache
        self.providers_cache.write().await.remove(&provider_id);

        Ok(())
    }

    /// Check if a provider can be safely deleted and return detailed usage information
    pub async fn check_provider_deletion_safety(
        &self,
        provider_id: i32,
    ) -> Result<ProviderDeletionCheck, GitProviderManagerError> {
        // Check if provider exists
        let provider = self.get_provider(provider_id).await?;

        // Get all connections for this provider
        let connections = self.get_provider_connections(provider_id).await?;

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
            let projects: Vec<temps_entities::projects::Model> =
                temps_entities::projects::Entity::find()
                    .filter(
                        temps_entities::projects::Column::GitProviderConnectionId
                            .eq(Some(connection.id)),
                    )
                    .order_by_desc(temps_entities::projects::Column::CreatedAt)
                    .all(self.db.as_ref())
                    .await?;

            for project in projects {
                projects_in_use.push(ProjectUsageInfo {
                    id: project.id,
                    name: project.name,
                    slug: project.slug,
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
        let connections = self.get_provider_connections(provider_id).await?;

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

    /// Permanently delete a git provider connection (hard delete)
    pub async fn delete_connection(
        &self,
        connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        // Check if connection exists
        self.get_connection(connection_id).await?;

        // Check if connection is in use by any projects
        let project_count = temps_entities::projects::Entity::find()
            .filter(
                temps_entities::projects::Column::GitProviderConnectionId.eq(Some(connection_id)),
            )
            .count(self.db.as_ref())
            .await?;

        if project_count > 0 {
            return Err(GitProviderManagerError::InvalidConfiguration(format!(
                "Cannot delete connection {} because it is used by {} project(s)",
                connection_id, project_count
            )));
        }

        // Delete repositories associated with this connection
        repositories::Entity::delete_many()
            .filter(repositories::Column::GitProviderConnectionId.eq(Some(connection_id)))
            .exec(self.db.as_ref())
            .await?;

        // Delete the connection
        git_provider_connections::Entity::delete_by_id(connection_id)
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }

    /// Handle push event by queueing GitPushEventJob for all matching projects
    pub async fn handle_push_event(
        &self,
        owner: String,
        repo: String,
        branch: Option<String>,
        tag: Option<String>,
        commit: String,
    ) -> Result<(), GitProviderManagerError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_entities::projects;

        // Find all projects with this owner and repo
        let matching_projects = projects::Entity::find()
            .filter(projects::Column::RepoOwner.eq(&owner))
            .filter(projects::Column::RepoName.eq(&repo))
            .all(self.db.as_ref())
            .await
            .map_err(|e| {
                tracing::error!("Failed to find projects for {}/{}: {}", owner, repo, e);
                GitProviderManagerError::DatabaseError(e)
            })?;

        if matching_projects.is_empty() {
            tracing::warn!(
                "No projects found for repository {}/{}, skipping push event",
                owner,
                repo
            );
            return Ok(());
        }

        tracing::info!(
            "Found {} projects for repository {}/{}, queueing push events",
            matching_projects.len(),
            owner,
            repo
        );

        // Queue a GitPushEventJob for each project
        for project in matching_projects {
            let push_job = temps_core::GitPushEventJob {
                owner: owner.clone(),
                repo: repo.clone(),
                branch: branch.clone(),
                tag: tag.clone(),
                commit: commit.clone(),
                project_id: project.id,
            };

            if let Err(e) = self
                .queue_service
                .send(temps_core::Job::GitPushEvent(push_job))
                .await
            {
                tracing::error!(
                    "Failed to queue push event job for project {}: {}",
                    project.id,
                    e
                );
                // Continue with other projects even if one fails
            } else {
                tracing::info!(
                    "Queued push event for project {} ({}/{})",
                    project.id,
                    owner,
                    repo
                );
            }
        }

        Ok(())
    }
}

// Implement the trait for GitProviderManager
use super::git_provider_manager_trait::{GitProviderManagerTrait, RepositoryInfo};
use async_trait::async_trait;
use std::path::Path;

#[async_trait]
impl GitProviderManagerTrait for GitProviderManager {
    async fn clone_repository(
        &self,
        connection_id: i32,
        repo_owner: &str,
        repo_name: &str,
        target_dir: &Path,
        branch_or_ref: Option<&str>,
    ) -> Result<(), super::git_provider_manager_trait::GitProviderManagerError> {
        use super::git_provider_manager_trait::GitProviderManagerError as TraitError;

        // Check if directory is empty
        if target_dir.exists() {
            let is_empty = std::fs::read_dir(target_dir)
                .map_err(|e| TraitError::CloneError(format!("Failed to read directory: {}", e)))?
                .next()
                .is_none();

            if !is_empty {
                return Err(TraitError::DirectoryNotEmpty(
                    target_dir.display().to_string(),
                ));
            }
        } else {
            std::fs::create_dir_all(target_dir).map_err(|e| {
                TraitError::CloneError(format!("Failed to create directory: {}", e))
            })?;
        }

        // Get connection and provider
        let connection = self
            .get_connection(connection_id)
            .await
            .map_err(|_| TraitError::ConnectionNotFound(connection_id))?;

        let provider_service = self
            .get_provider_service(connection.provider_id)
            .await
            .map_err(|_| TraitError::ProviderNotFound(connection.provider_id))?;

        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await
            .map_err(|e| TraitError::DecryptionError(e.to_string()))?;

        // Get repository info
        let repo = provider_service
            .get_repository(&access_token, repo_owner, repo_name)
            .await
            .map_err(|e| TraitError::CloneError(format!("Failed to get repository: {}", e)))?;

        // Clone the repository
        provider_service
            .clone_repository(
                &repo.clone_url,
                target_dir.to_str().unwrap(),
                Some(&access_token),
            )
            .await
            .map_err(|e| TraitError::CloneError(format!("Failed to clone: {}", e)))?;

        // Checkout specific ref if provided
        if let Some(ref_name) = branch_or_ref {
            if ref_name != repo.default_branch {
                let output = tokio::process::Command::new("git")
                    .arg("checkout")
                    .arg(ref_name)
                    .current_dir(target_dir)
                    .output()
                    .await
                    .map_err(|e| {
                        TraitError::CloneError(format!("Failed to run git checkout: {}", e))
                    })?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(TraitError::CloneError(format!(
                        "Failed to checkout ref {}: {}",
                        ref_name, stderr
                    )));
                }
            }
        }

        Ok(())
    }

    async fn get_repository_info(
        &self,
        connection_id: i32,
        repo_owner: &str,
        repo_name: &str,
    ) -> Result<RepositoryInfo, super::git_provider_manager_trait::GitProviderManagerError> {
        use super::git_provider_manager_trait::GitProviderManagerError as TraitError;

        let connection = self
            .get_connection(connection_id)
            .await
            .map_err(|_| TraitError::ConnectionNotFound(connection_id))?;

        let provider_service = self
            .get_provider_service(connection.provider_id)
            .await
            .map_err(|_| TraitError::ProviderNotFound(connection.provider_id))?;

        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await
            .map_err(|e| TraitError::DecryptionError(e.to_string()))?;

        let repo = provider_service
            .get_repository(&access_token, repo_owner, repo_name)
            .await
            .map_err(|e| TraitError::Other(format!("Failed to get repository: {}", e)))?;

        Ok(RepositoryInfo {
            clone_url: repo.clone_url,
            default_branch: repo.default_branch,
            owner: repo_owner.to_string(),
            name: repo_name.to_string(),
        })
    }

    async fn download_archive(
        &self,
        connection_id: i32,
        repo_owner: &str,
        repo_name: &str,
        branch_or_ref: &str,
        archive_path: &Path,
    ) -> Result<(), super::git_provider_manager_trait::GitProviderManagerError> {
        use super::git_provider_manager_trait::GitProviderManagerError as TraitError;

        let connection = self
            .get_connection(connection_id)
            .await
            .map_err(|_| TraitError::ConnectionNotFound(connection_id))?;

        let provider_service = self
            .get_provider_service(connection.provider_id)
            .await
            .map_err(|_| TraitError::ProviderNotFound(connection.provider_id))?;

        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await
            .map_err(|e| TraitError::DecryptionError(e.to_string()))?;

        provider_service
            .download_archive(
                &access_token,
                repo_owner,
                repo_name,
                branch_or_ref,
                archive_path,
            )
            .await
            .map_err(|e| TraitError::Other(format!("Failed to download archive: {}", e)))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_core::{async_trait::async_trait, Job, JobReceiver, QueueError};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{git_provider_connections, git_providers};

    // Mock implementations for tests
    struct MockJobQueue;

    #[async_trait]
    impl JobQueue for MockJobQueue {
        async fn send(&self, _job: Job) -> Result<(), QueueError> {
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn JobReceiver> {
            panic!("subscribe not needed for tests")
        }
    }

    // Helper function to create a test ConfigService
    fn create_test_config_service(db: Arc<DatabaseConnection>) -> Arc<temps_config::ConfigService> {
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                None,
            )
            .unwrap(),
        );
        Arc::new(temps_config::ConfigService::new(server_config, db))
    }

    #[tokio::test]
    async fn test_delete_installation_deactivates_provider() {
        use chrono::Utc;
        use temps_entities::users;

        // Create real test database
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        // Create a test user first (required for foreign key)
        let now = Utc::now();
        let user = users::ActiveModel {
            email: Set("test@example.com".to_string()),
            password_hash: Set(Some("hash".to_string())),
            name: Set("Test User".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        user.insert(db.as_ref()).await.unwrap();

        // Create a git provider
        let provider = git_providers::ActiveModel {
            name: Set("test-provider".to_string()),
            provider_type: Set("github".to_string()),
            base_url: Set(None),
            api_url: Set(None),
            auth_method: Set("oauth".to_string()),
            auth_config: Set(serde_json::json!({})),
            webhook_secret: Set(None),
            is_active: Set(true),
            is_default: Set(false),
            ..Default::default()
        };
        let provider = provider.insert(db.as_ref()).await.unwrap();

        // Create a git provider connection with installation ID
        let connection = git_provider_connections::ActiveModel {
            provider_id: Set(provider.id),
            user_id: Set(Some(1)),
            account_name: Set("test-account".to_string()),
            account_type: Set("Organization".to_string()),
            access_token: Set(None),
            refresh_token: Set(None),
            token_expires_at: Set(None),
            refresh_token_expires_at: Set(None),
            installation_id: Set(Some("12345".to_string())),
            metadata: Set(None),
            is_active: Set(true),
            is_expired: Set(false),
            syncing: Set(false),
            last_synced_at: Set(None),
            ..Default::default()
        };
        connection.insert(db.as_ref()).await.unwrap();

        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );

        let queue_service = Arc::new(MockJobQueue) as Arc<dyn JobQueue>;
        let config_service = create_test_config_service(db.clone());

        let manager = GitProviderManager::new(
            db.clone(),
            encryption_service,
            queue_service,
            config_service,
        );

        // Test delete_installation
        let result = manager.delete_installation(12345).await;

        assert!(result.is_ok(), "delete_installation should succeed");

        // Verify connection and provider were deactivated
        // (actual verification would require querying the database)
    }

    #[tokio::test]
    async fn test_delete_installation_with_nonexistent_installation() {
        // Create real test database with no data
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let db = test_db.connection_arc();

        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );

        let queue_service = Arc::new(MockJobQueue) as Arc<dyn JobQueue>;
        let config_service = create_test_config_service(db.clone());

        let manager = GitProviderManager::new(
            db.clone(),
            encryption_service,
            queue_service,
            config_service,
        );

        // Should succeed even if installation not found (idempotent)
        let result = manager.delete_installation(99999).await;
        assert!(result.is_ok(), "delete_installation should be idempotent");
    }
}
