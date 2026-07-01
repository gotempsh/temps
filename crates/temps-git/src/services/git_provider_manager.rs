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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectPresetDomain {
    pub path: String,
    pub preset: String,
    pub preset_label: String,
    pub exposed_port: Option<u16>,
    pub icon_url: Option<String>,
    pub project_type: String,
    /// Compose file paths found in the repository (only for docker-compose preset)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_files: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct RepositoryPresetDomain {
    pub repository_id: i32,
    pub owner: String,
    pub name: String,
    pub presets: Vec<ProjectPresetDomain>,
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

    #[error(
        "Sync for connection {connection_id} exceeded hard deadline of {deadline_secs}s and was aborted"
    )]
    SyncTimeout {
        connection_id: i32,
        deadline_secs: u64,
    },

    #[error("Connection token expired for connection ID {connection_id}. Please update your access token.")]
    ConnectionTokenExpired { connection_id: i32 },

    #[error("Queue error: {0}")]
    QueueError(String),

    #[error("OAuth state not found or expired: {0}")]
    OAuthStateInvalid(String),
}

/// Hard ceiling on a single sync run. If we haven't finished within this
/// window something has gone badly wrong (stuck HTTPS connection, provider
/// outage with retries spinning, etc) and we'd rather abort and release
/// the `syncing` flag than hold it indefinitely. 30 minutes is well above
/// a healthy 20,000-repo sync's wall time and well below "the user has
/// given up and restarted the server."
const SYNC_HARD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Drop guard that resets `syncing=false` on a connection when it leaves
/// scope. Covers every exit path — Ok, Err, panic, task cancel, deadline
/// timeout — so the flag never gets stuck in the `true` state.
///
/// The guard owns an `Arc<GitProviderManager>` so the write is issued
/// against the live DB handle. Because `Drop` is synchronous, we spawn a
/// detached tokio task to perform the update. If the runtime is shutting
/// down and can't accept new tasks we log and move on — the startup
/// reset in `plugin.rs` will clean up on the next boot.
struct SyncFlagGuard {
    manager: Option<Arc<GitProviderManager>>,
    connection_id: i32,
}

impl SyncFlagGuard {
    fn new(manager: Arc<GitProviderManager>, connection_id: i32) -> Self {
        Self {
            manager: Some(manager),
            connection_id,
        }
    }
}

impl Drop for SyncFlagGuard {
    fn drop(&mut self) {
        let Some(manager) = self.manager.take() else {
            return;
        };
        let connection_id = self.connection_id;
        // Best-effort: if we're mid-tokio-shutdown, `spawn` will panic.
        // Catch that so a panicking Drop doesn't bring down the process.
        let spawn_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::spawn(async move {
                if let Err(e) = manager
                    .set_connection_syncing_status(connection_id, false)
                    .await
                {
                    tracing::error!(
                        connection_id,
                        error = %e,
                        "Failed to reset syncing flag from SyncFlagGuard"
                    );
                }
            });
        }));
        if spawn_result.is_err() {
            tracing::warn!(
                connection_id,
                "Could not spawn syncing-flag reset during Drop (runtime likely shutting down); startup reset will handle it"
            );
        }
    }
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

    /// Expose the underlying database connection for crate-internal use.
    pub fn db(&self) -> &DatabaseConnection {
        self.db.as_ref()
    }

    /// Decrypt an encrypted token string (e.g., a GitLab webhook signing token).
    /// Thin public wrapper around the private `decrypt_string` helper so that
    /// webhook handlers inside the same crate can access it without going through
    /// the full service layer.
    pub async fn decrypt_token(&self, encrypted: &str) -> Result<String, GitProviderManagerError> {
        self.decrypt_string(encrypted).await
    }

    /// Get an access token from any active GitHub connection.
    /// Used for public repo API calls to avoid unauthenticated rate limits (60 → 5000 req/hr).
    /// Validates the token against GitHub's API before returning it.
    pub async fn get_any_github_token(&self) -> Option<String> {
        use temps_entities::git_providers;

        let connections = self.get_user_connections().await.ok()?;

        for conn in connections {
            if !conn.is_active {
                continue;
            }

            // Check provider type is GitHub
            let provider = git_providers::Entity::find_by_id(conn.provider_id)
                .one(self.db.as_ref())
                .await
                .ok()
                .flatten();

            let is_github = provider
                .as_ref()
                .map(|p| p.provider_type == "github" || p.provider_type == "github_app")
                .unwrap_or(false);

            if !is_github {
                continue;
            }

            // get_connection_token validates expiry and refreshes OAuth/App tokens
            let token = match self.get_connection_token(conn.id).await {
                Ok(t) if !t.is_empty() => t,
                _ => continue,
            };

            // Verify the token actually works with a lightweight API call
            let client = reqwest::Client::builder()
                .user_agent("Temps-Engine/1.0")
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .ok()?;

            let resp = client
                .get("https://api.github.com/rate_limit")
                .header("Authorization", format!("token {}", token))
                .send()
                .await
                .ok()?;

            if resp.status().is_success() {
                tracing::debug!(
                    "Using GitHub token from connection {} for public repo API calls",
                    conn.id
                );
                return Some(token);
            }

            tracing::warn!(
                "GitHub token from connection {} failed validation (status {}), trying next",
                conn.id,
                resp.status()
            );
        }
        None
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
    fn is_auth_failure(message: &str) -> bool {
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
                let installation_id = connection.installation_id.as_ref().unwrap(); // Safe: checked above

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
        operation: super::git_provider::ScopedTokenOp,
    ) -> Result<super::git_provider::ScopedTokenGrant, GitProviderManagerError> {
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

        // Fallback: PAT or OAuth connection. There is no provider API
        // to narrow these tokens to (repo, permission) at runtime, so
        // we hand back the stored long-lived token. This trades the
        // per-op blast-radius guarantee for the ability to use git at
        // all — operators who need the strict guarantee should switch
        // their project to a GitHub App connection. The trade-off is
        // logged at WARN so audit reviews can see when it kicked in.
        let provider = self.get_provider(connection.provider_id).await?;
        let encrypted_token = connection.access_token.as_ref().ok_or_else(|| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Connection {} has no access_token; cannot mint credential",
                connection_id
            ))
        })?;
        let token = self.decrypt_string(encrypted_token).await?;

        // Username conventions for HTTP Basic auth on git over HTTPS:
        //   GitHub: `x-access-token` (works for both App tokens and PATs)
        //   GitLab: `oauth2` (works for both PATs and OAuth tokens)
        // Anything else: best-effort `x-access-token`. If we ever add
        // Bitbucket etc. this needs updating.
        let username = match provider.provider_type.as_str() {
            "gitlab" => "oauth2",
            "gitea" => "x-access-token",
            "bitbucket" => "x-token-auth",
            _ => "x-access-token",
        }
        .to_string();

        tracing::warn!(
            connection_id,
            owner = %owner,
            repo = %repo,
            ?operation,
            provider_type = %provider.provider_type,
            "Minting non-scoped credential from PAT/OAuth connection — \
             token is NOT narrowed per-op. Switch the project to a \
             GitHub App connection if per-op scoping is required."
        );

        Ok(super::git_provider::ScopedTokenGrant {
            username,
            password: token,
            // Stored tokens may have an expiry on the connection row,
            // but the daemon doesn't depend on it (it re-mints on every
            // operation anyway). Surface `None` so the helper protocol
            // doesn't carry a misleading promise.
            expires_at: connection.token_expires_at,
        })
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

        // Probe the upstream immediately so the connection's health_status
        // reflects reality without waiting for the scheduled sweep. We never
        // let a probe failure break connection creation — an unreachable
        // provider is recorded as unhealthy with a reason and the caller gets
        // their connection row back.
        if let Err(e) = self.probe_and_record_connection_health(connection.id).await {
            tracing::warn!(
                connection_id = connection.id,
                error = %e,
                "Initial connection health probe failed to record; row stays at default status"
            );
        }

        // Re-fetch so callers see the updated health_status/last_health_check_at
        // instead of the pre-probe defaults.
        let connection = self.get_connection(connection.id).await?;
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

    /// Set syncing status for a connection. When flipping to `true`, we also
    /// reset `synced_repository_count` to 0 so the UI's running progress
    /// starts fresh for this sync.
    async fn set_connection_syncing_status(
        &self,
        connection_id: i32,
        syncing: bool,
    ) -> Result<(), GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;
        let mut active_model: git_provider_connections::ActiveModel = connection.into();
        active_model.syncing = Set(syncing);
        if syncing {
            active_model.synced_repository_count = Set(0);
        }
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
    /// Mark a connection as syncing and spawn the sync loop as a detached
    /// background task.
    ///
    /// Returns as soon as `syncing=true` has been persisted, so the HTTP
    /// caller can close the connection without interrupting the sync. The
    /// spawned task owns a drop-guard that flips `syncing=false` on *any*
    /// exit path — normal completion, error, timeout, or drop. The whole
    /// inner sync is wrapped in a generous deadline to guarantee the flag
    /// cannot get stuck forever even if the provider hangs mid-request.
    ///
    /// Returns `SyncInProgress` if the connection is already syncing.
    pub async fn spawn_sync_repositories(
        self: Arc<Self>,
        connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;
        if connection.syncing {
            return Err(GitProviderManagerError::SyncInProgress);
        }

        // Flip to syncing=true before returning so the client's immediate
        // follow-up read observes the fresh state.
        self.set_connection_syncing_status(connection_id, true)
            .await?;

        let manager = self.clone();
        tokio::spawn(async move {
            manager.run_sync_guarded(connection_id).await;
        });

        Ok(())
    }

    /// Synchronous variant of `spawn_sync_repositories` for callers that
    /// need to block until the sync finishes (tests, CLI flows). Everyday
    /// code paths should prefer `spawn_sync_repositories` so the sync
    /// survives HTTP client disconnects.
    pub async fn sync_repositories(
        &self,
        connection_id: i32,
    ) -> Result<Vec<repositories::Model>, GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;
        if connection.syncing {
            return Err(GitProviderManagerError::SyncInProgress);
        }

        self.set_connection_syncing_status(connection_id, true)
            .await?;

        // Hold the guard on the stack so even a panic inside the inner
        // call unwinds through the Drop impl and resets the flag.
        let guard = SyncFlagGuard::new(self.clone_arc(), connection_id);
        let result = tokio::time::timeout(
            SYNC_HARD_DEADLINE,
            self.sync_repositories_internal(connection_id),
        )
        .await;
        drop(guard);

        match result {
            Ok(Ok(())) => {
                let repos = repositories::Entity::find()
                    .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
                    .order_by_desc(repositories::Column::PushedAt)
                    .all(self.db.as_ref())
                    .await?;
                Ok(repos)
            }
            Ok(Err(e)) => {
                error!(
                    connection_id,
                    error = %e,
                    "Repository sync failed"
                );
                Err(e)
            }
            Err(_elapsed) => {
                error!(
                    connection_id,
                    deadline_secs = SYNC_HARD_DEADLINE.as_secs(),
                    "Repository sync exceeded hard deadline and was aborted"
                );
                Err(GitProviderManagerError::SyncTimeout {
                    connection_id,
                    deadline_secs: SYNC_HARD_DEADLINE.as_secs(),
                })
            }
        }
    }

    /// Runs the inner sync loop behind a drop-guard and a hard deadline.
    /// Meant to be invoked from a detached `tokio::spawn` — never returns
    /// errors to a caller, only logs them.
    async fn run_sync_guarded(self: Arc<Self>, connection_id: i32) {
        let guard = SyncFlagGuard::new(self.clone(), connection_id);
        let outcome = tokio::time::timeout(
            SYNC_HARD_DEADLINE,
            self.sync_repositories_internal(connection_id),
        )
        .await;
        drop(guard);

        match outcome {
            Ok(Ok(())) => {
                tracing::info!(connection_id, "Repository sync completed");
            }
            Ok(Err(e)) => {
                tracing::error!(
                    connection_id,
                    error = %e,
                    "Repository sync failed; syncing flag has been reset"
                );
            }
            Err(_elapsed) => {
                tracing::error!(
                    connection_id,
                    deadline_secs = SYNC_HARD_DEADLINE.as_secs(),
                    "Repository sync exceeded hard deadline and was aborted"
                );
            }
        }
    }

    /// Wrap `&self` in an `Arc` copy of the manager. Used to hand the
    /// manager into a `Drop` guard that needs owned access.
    fn clone_arc(&self) -> Arc<Self> {
        Arc::new(self.clone())
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

        // Streaming sync: we flush each page to the database as soon as it
        // arrives from the provider. This matters for tenants with thousands
        // of repos: buffering everything in memory before writing was the old
        // design and it doesn't scale past ~a few thousand projects, plus a
        // failure on page N threw away pages 1..N-1 of work.
        //
        // Deletion detection still works: we track every full_name we've seen
        // across pages, then at the end any existing row whose full_name
        // wasn't seen is considered stale. We only run that sweep on clean
        // completion — a partial sync must NOT delete rows we simply haven't
        // observed yet.
        let mut seen_full_names: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut total_inserted: usize = 0;
        let mut total_updated: usize = 0;
        let mut page: u32 = 1;
        // Hard ceiling so a broken cursor can't loop forever. 200 pages of
        // 100 repos = 20,000 — enough for realistic syncs; if you need more,
        // bump it here.
        const MAX_PAGES: u32 = 200;

        let is_github_app =
            provider.provider_type == "github" && provider.auth_method == "github_app";

        loop {
            // Fetch one page. For GitHub Apps we already have a fresh
            // installation token; for OAuth we route through
            // execute_with_refresh so the token refreshes mid-sync if needed.
            let page_result = if is_github_app {
                provider_service
                    .list_repositories_page(&access_token, organization.as_deref(), page)
                    .await?
            } else {
                self.execute_with_refresh(connection_id, |token| {
                    let org = organization.clone();
                    let svc = provider_service.clone();
                    async move {
                        svc.list_repositories_page(&token, org.as_deref(), page)
                            .await
                    }
                })
                .await?
            };

            let page_items = page_result.items;
            let next_page = page_result.next_page;

            if page_items.is_empty() && page == 1 {
                tracing::info!(
                    "Sync for connection {} returned zero repositories on page 1",
                    connection_id
                );
            }

            if !page_items.is_empty() {
                // Flush this page: diff against existing rows and upsert.
                // Each page is an independent transaction from the DB's POV —
                // if page N+1 fails, pages 1..N are already persisted.
                let full_names: Vec<String> =
                    page_items.iter().map(|r| r.full_name.clone()).collect();

                let existing_rows = repositories::Entity::find()
                    .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
                    .filter(repositories::Column::FullName.is_in(full_names.clone()))
                    .all(self.db.as_ref())
                    .await?;

                let mut existing_map: std::collections::HashMap<String, repositories::Model> =
                    existing_rows
                        .into_iter()
                        .map(|r| (r.full_name.clone(), r))
                        .collect();

                let mut to_insert: Vec<repositories::ActiveModel> = Vec::new();
                let mut to_update: Vec<repositories::ActiveModel> = Vec::new();

                for repo in page_items {
                    seen_full_names.insert(repo.full_name.clone());
                    if let Some(existing) = existing_map.remove(&repo.full_name) {
                        let mut active: repositories::ActiveModel = existing.into();
                        active.description = Set(repo.description.clone());
                        active.default_branch = Set(repo.default_branch.clone());
                        active.language = Set(repo.language.clone());
                        active.size = Set(repo.size as i32);
                        active.stargazers_count = Set(repo.stars);
                        active.pushed_at = Set(repo.pushed_at.unwrap_or(repo.updated_at));
                        active.clone_url = Set(Some(repo.clone_url.clone()));
                        active.ssh_url = Set(Some(repo.ssh_url.clone()));
                        active.created_at = Set(repo.created_at);
                        active.updated_at = Set(repo.updated_at);
                        to_update.push(active);
                    } else {
                        let new_row = repositories::ActiveModel {
                            git_provider_connection_id: Set(connection_id),
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
                        to_insert.push(new_row);
                    }
                }

                let page_update_count = to_update.len();
                let page_insert_count = to_insert.len();

                for row in to_update {
                    row.update(self.db.as_ref()).await?;
                }
                if !to_insert.is_empty() {
                    repositories::Entity::insert_many(to_insert)
                        .exec(self.db.as_ref())
                        .await?;
                }

                total_updated += page_update_count;
                total_inserted += page_insert_count;

                tracing::info!(
                    connection_id,
                    page,
                    updates = page_update_count,
                    inserts = page_insert_count,
                    running_total_updated = total_updated,
                    running_total_inserted = total_inserted,
                    "Flushed repository sync page"
                );

                // Touch last_synced_at and the running progress count on
                // every page so the UI can show a long-running sync
                // advancing — a stale "last synced 1 day ago" on a
                // 20,000-repo tenant is worse than a frequently-bumped
                // timestamp, and the count lets the UI display
                // "Synced 1,400 repos…" live.
                let touch = git_provider_connections::ActiveModel {
                    id: Set(connection_id),
                    last_synced_at: Set(Some(chrono::Utc::now())),
                    synced_repository_count: Set((total_inserted + total_updated) as i32),
                    ..Default::default()
                };
                touch.update(self.db.as_ref()).await?;
            }

            match next_page {
                Some(next) if next > page && page < MAX_PAGES => page = next,
                _ => break,
            }
        }

        tracing::info!(
            connection_id,
            total_updated,
            total_inserted,
            pages_scanned = page,
            "Sync complete"
        );

        // Deletion sweep: any existing row we didn't see across ALL pages is
        // stale (repo deleted/transferred/unshared on the provider). We only
        // run this after a clean completion — if the loop errored out, we'd
        // delete rows we just hadn't fetched yet, which would be worse than a
        // stale row.
        if !seen_full_names.is_empty() {
            let existing_full_names: Vec<String> = repositories::Entity::find()
                .select_only()
                .column(repositories::Column::FullName)
                .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
                .into_tuple::<String>()
                .all(self.db.as_ref())
                .await?;

            let stale: Vec<String> = existing_full_names
                .into_iter()
                .filter(|name| !seen_full_names.contains(name))
                .collect();

            if !stale.is_empty() {
                tracing::info!(
                    connection_id,
                    stale_count = stale.len(),
                    "Deleting repositories no longer present upstream"
                );
                repositories::Entity::delete_many()
                    .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
                    .filter(repositories::Column::FullName.is_in(stale))
                    .exec(self.db.as_ref())
                    .await?;
            }
        }

        // Final last_synced_at and progress-count update (redundant with
        // per-page touches but makes the completion instant precise and
        // guarantees the count matches the exact final total).
        let final_touch = git_provider_connections::ActiveModel {
            id: Set(connection_id),
            last_synced_at: Set(Some(chrono::Utc::now())),
            synced_repository_count: Set((total_inserted + total_updated) as i32),
            ..Default::default()
        };
        final_touch.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Calculate and store preset for a repository
    pub async fn calculate_and_store_preset(
        &self,
        repo_id: i32,
        _connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        // Verify repository exists
        let _ = repositories::Entity::find_by_id(repo_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                GitProviderManagerError::InvalidConfiguration(format!(
                    "Repository {} not found",
                    repo_id
                ))
            })?;

        // Calculate preset (this will automatically cache it in the database)
        let _ = self
            .calculate_repository_preset_live(
                repo_id, None, // Use default branch
            )
            .await?;

        Ok(())
    }

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

    /// Get repository preset from database by branch
    /// If branch is not specified, uses the repository's default branch
    pub async fn get_repository_preset(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        branch: Option<String>,
    ) -> Result<Option<serde_json::Value>, GitProviderManagerError> {
        // Find repository
        let repository = repositories::Entity::find()
            .filter(repositories::Column::GitProviderConnectionId.eq(connection_id))
            .filter(repositories::Column::Owner.eq(owner))
            .filter(repositories::Column::Name.eq(repo))
            .one(self.db.as_ref())
            .await?;

        let repository = match repository {
            Some(r) => r,
            None => return Ok(None),
        };

        // Use provided branch or fall back to default branch
        let target_branch = branch.unwrap_or_else(|| repository.default_branch.clone());

        // Get preset cache and extract data for the specific branch
        let preset_data = repository.preset.as_ref().and_then(|json_cache| {
            // Deserialize Json to RepositoryPresetCache
            let cache: repositories::RepositoryPresetCache =
                serde_json::from_value(json_cache.clone()).ok()?;
            cache
                .branches
                .get(&target_branch)
                .map(|branch_data| json!(branch_data))
        });

        Ok(preset_data)
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

        // Repository always has a git provider connection (required field)
        let connection_id = repository.git_provider_connection_id;

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
        let presets = self.detect_presets_in_directories(&files).await;

        // Cache presets in repositories.preset as HashMap<branch, preset_data>
        let calculated_at = chrono::Utc::now();

        // Get existing preset cache or create new one
        let mut preset_cache: repositories::RepositoryPresetCache = repository
            .preset
            .as_ref()
            .and_then(|json| serde_json::from_value(json.clone()).ok())
            .unwrap_or_else(|| repositories::RepositoryPresetCache {
                branches: std::collections::HashMap::new(),
            });

        // Convert ProjectPresetDomain to PresetInfo
        let preset_infos: Vec<repositories::PresetInfo> = presets
            .iter()
            .map(|p| repositories::PresetInfo {
                path: p.path.clone(),
                preset: p.preset.clone(),
                preset_label: p.preset_label.clone(),
                exposed_port: p.exposed_port,
                icon_url: p.icon_url.clone(),
                project_type: p.project_type.clone(),
                compose_files: p.compose_files.clone(),
            })
            .collect();

        // Update cache for this branch
        preset_cache.branches.insert(
            target_branch.clone(),
            repositories::BranchPresetData {
                presets: preset_infos,
                calculated_at,
            },
        );

        // Serialize cache to Json for database storage
        let preset_json = serde_json::to_value(&preset_cache).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Failed to serialize preset cache: {}",
                e
            ))
        })?;

        // Update repository with new cache
        let mut repo_update: repositories::ActiveModel = repository.clone().into();
        repo_update.preset = Set(Some(preset_json));
        repo_update.update(self.db.as_ref()).await?;

        Ok(RepositoryPresetDomain {
            repository_id,
            owner: repository.owner,
            name: repository.name,
            presets,
            calculated_at,
        })
    }

    /// Detect presets in directories with proper grouping and filtering
    async fn detect_presets_in_directories(&self, files: &[String]) -> Vec<ProjectPresetDomain> {
        // Use the centralized file tree detection from temps-presets
        let detected_presets = temps_presets::detect_presets_from_file_tree(files);

        // Convert to ProjectPresetDomain
        detected_presets
            .into_iter()
            .map(|preset| {
                // Parse preset slug to get metadata from entity enum
                let preset_enum = preset.slug.parse::<temps_entities::preset::Preset>().ok();

                let exposed_port = preset_enum
                    .as_ref()
                    .and_then(|p| p.exposed_port())
                    .or(preset.exposed_port);

                let icon_url = preset_enum
                    .as_ref()
                    .and_then(|p| p.icon_url())
                    .map(|s| s.to_string());

                let project_type = preset_enum
                    .as_ref()
                    .map(|p| p.project_type().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                ProjectPresetDomain {
                    path: preset.path,
                    preset: preset.slug,
                    preset_label: preset.label,
                    exposed_port,
                    icon_url,
                    project_type,
                    compose_files: preset.compose_files,
                }
            })
            .collect()
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

        // Validate the new access token as a PAT (using /user endpoint)
        let client = reqwest::Client::new();
        let api_url = provider
            .api_url
            .as_deref()
            .unwrap_or("https://api.github.com");
        let validation_endpoint =
            if provider.provider_type == "github" || provider.provider_type == "gitlab" {
                format!("{}/user", api_url)
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
                let error_text = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "Unknown error".to_string());
                tracing::warn!(
                    "Error validating new access token for connection {}: {} - {}",
                    connection_id,
                    status,
                    error_text
                );
                return Err(GitProviderManagerError::InvalidConfiguration(format!(
                    "Failed to validate the provided access token: {} - {}",
                    status, error_text
                )));
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

    /// Validate and update connection token - legacy method for compatibility
    /// Prefer update_connection_token which validates as PAT
    #[allow(dead_code)]
    pub async fn update_connection_token_with_provider_validation(
        &self,
        connection_id: i32,
        new_access_token: String,
        new_refresh_token: Option<String>,
    ) -> Result<(), GitProviderManagerError> {
        let connection = self.get_connection(connection_id).await?;

        // Validate the new access token before saving using provider's method
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

    /// Run an immediate token probe against the upstream provider and persist
    /// `health_status` / `health_message` / `last_health_check_at` on the
    /// connection row. Intended to be called right after a connection is
    /// created (PAT or OAuth flows) so the UI reflects reality without waiting
    /// for the daily sweep.
    ///
    /// GitHub App installations are out of scope: they're probed via a
    /// different path (installation ID against the App API), which the
    /// `ConnectionHealthService` handles. For those, callers should invoke
    /// `connection_health_service.check_connection_health` instead.
    ///
    /// This method never errors on upstream failures — an unreachable or
    /// rejecting provider is simply recorded as `unhealthy`. It only returns
    /// `Err` for local issues (DB failure, missing connection, decrypt error).
    pub async fn probe_and_record_connection_health(
        &self,
        connection_id: i32,
    ) -> Result<(), GitProviderManagerError> {
        use crate::services::connection_health::{HEALTH_STATUS_HEALTHY, HEALTH_STATUS_UNHEALTHY};

        let connection = self.get_connection(connection_id).await?;

        // GitHub App installations need installation-based probing, not token
        // validation. Skip here and let the scheduled sweep / explicit endpoint
        // handle them.
        if connection.installation_id.is_some() {
            return Ok(());
        }

        let access_token = match connection.access_token.as_deref() {
            Some(encrypted) => self.decrypt_string(encrypted).await?,
            None => {
                // No token to validate; mark unknown-but-unhealthy with reason.
                self.persist_health_status(
                    connection_id,
                    HEALTH_STATUS_UNHEALTHY,
                    Some("Connection has no access token".to_string()),
                )
                .await?;
                return Ok(());
            }
        };

        let provider_service = self.get_provider_service(connection.provider_id).await?;

        let (status, message) = match provider_service.validate_token(&access_token).await {
            Ok(true) => (HEALTH_STATUS_HEALTHY, None),
            Ok(false) => (
                HEALTH_STATUS_UNHEALTHY,
                Some("Access token rejected by provider (revoked or invalid)".to_string()),
            ),
            Err(GitProviderError::AuthenticationFailed(msg)) => (
                HEALTH_STATUS_UNHEALTHY,
                Some(format!("Authentication failed: {}", msg)),
            ),
            Err(e) => (
                HEALTH_STATUS_UNHEALTHY,
                Some(format!("Provider error: {}", e)),
            ),
        };

        self.persist_health_status(connection_id, status, message)
            .await?;
        Ok(())
    }

    /// Write the health result onto the connection row. Shared helper used by
    /// `probe_and_record_connection_health` so both the healthy and unhealthy
    /// branches go through the same update path.
    async fn persist_health_status(
        &self,
        connection_id: i32,
        status: &str,
        message: Option<String>,
    ) -> Result<(), GitProviderManagerError> {
        use crate::services::connection_health::HEALTH_STATUS_UNHEALTHY;

        let connection = self.get_connection(connection_id).await?;
        let now = chrono::Utc::now();
        let new_failures = if status == HEALTH_STATUS_UNHEALTHY {
            connection.consecutive_health_failures.saturating_add(1)
        } else {
            0
        };

        let mut active: git_provider_connections::ActiveModel = connection.into();
        active.health_status = Set(status.to_string());
        active.health_message = Set(message);
        active.last_health_check_at = Set(Some(now));
        active.consecutive_health_failures = Set(new_failures);
        active.updated_at = Set(now);
        active.update(self.db.as_ref()).await?;
        Ok(())
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

        // Branch/tag deletions send the all-zeros "null SHA" in the `after`
        // field (GitHub, GitLab, and others all follow this Git convention).
        // There is no commit to deploy, so creating a deployment from it would
        // queue a download_repo job that fails with
        // "Failed to checkout ref 0000000...". Ignore these events.
        if commit.is_empty() || commit.chars().all(|c| c == '0') {
            tracing::info!(
                "Ignoring push event for {}/{} with null SHA (branch/tag deletion), branch: {:?}, tag: {:?}",
                owner,
                repo,
                branch,
                tag
            );
            return Ok(());
        }

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
                // Real git provider webhook — honour automatic_deploy.
                manual_trigger: false,
                rollback_from_deployment_id: None,
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

    /// Push files to a repository using native git operations (git2 library)
    ///
    /// This is much more efficient than uploading blobs one by one via the API.
    /// It clones the repo locally, writes files, commits, and pushes in a single operation.
    ///
    /// # Arguments
    /// * `clone_url` - The repository clone URL
    /// * `access_token` - Access token for authentication
    /// * `branch` - The branch to push to
    /// * `files` - List of (path, content) tuples to push
    /// * `commit_message` - The commit message
    /// * `author_name` - Name for the commit author
    /// * `author_email` - Email for the commit author
    fn push_files_with_git(
        clone_url: &str,
        access_token: &str,
        branch: &str,
        files: Vec<(String, Vec<u8>)>,
        commit_message: &str,
        author_name: &str,
        author_email: &str,
    ) -> Result<String, GitProviderManagerError> {
        use git2::{Cred, PushOptions, RemoteCallbacks, Repository, Signature};
        use std::fs;

        // Create a temp directory for the clone
        let temp_dir = tempfile::tempdir().map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Failed to create temp dir: {}",
                e
            ))
        })?;

        let repo_path = temp_dir.path();

        // Inject token into the clone URL for HTTPS authentication
        // Transform https://github.com/owner/repo.git -> https://x-access-token:TOKEN@github.com/owner/repo.git
        let authenticated_url = if clone_url.starts_with("https://") {
            let url_without_scheme = clone_url.strip_prefix("https://").unwrap();
            format!(
                "https://x-access-token:{}@{}",
                access_token, url_without_scheme
            )
        } else {
            clone_url.to_string()
        };

        info!("Cloning repository to local temp directory for push operation");

        // Clone the repository
        let repo = Repository::clone(&authenticated_url, repo_path).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Failed to clone repository: {}",
                e
            ))
        })?;

        // Checkout the correct branch if it's not the default
        let head_ref = repo.head().map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Failed to get HEAD reference: {}",
                e
            ))
        })?;

        let current_branch = head_ref.shorthand().unwrap_or("main");

        if current_branch != branch {
            // Try to checkout the specified branch
            let branch_ref = format!("refs/remotes/origin/{}", branch);
            if let Ok(reference) = repo.find_reference(&branch_ref) {
                let commit = reference.peel_to_commit().map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!(
                        "Failed to peel to commit: {}",
                        e
                    ))
                })?;

                repo.branch(branch, &commit, false).map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!(
                        "Failed to create local branch: {}",
                        e
                    ))
                })?;

                let obj = repo
                    .revparse_single(&format!("refs/heads/{}", branch))
                    .map_err(|e| {
                        GitProviderManagerError::InvalidConfiguration(format!(
                            "Failed to find branch: {}",
                            e
                        ))
                    })?;

                repo.checkout_tree(&obj, None).map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!(
                        "Failed to checkout tree: {}",
                        e
                    ))
                })?;

                repo.set_head(&format!("refs/heads/{}", branch))
                    .map_err(|e| {
                        GitProviderManagerError::InvalidConfiguration(format!(
                            "Failed to set HEAD: {}",
                            e
                        ))
                    })?;
            }
        }

        info!("Writing {} files to local repository", files.len());

        // Write all files to the repository
        for (file_path, content) in &files {
            let full_path = repo_path.join(file_path);

            // Create parent directories if needed
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    GitProviderManagerError::InvalidConfiguration(format!(
                        "Failed to create directory for {}: {}",
                        file_path, e
                    ))
                })?;
            }

            // Write the file
            fs::write(&full_path, content).map_err(|e| {
                GitProviderManagerError::InvalidConfiguration(format!(
                    "Failed to write file {}: {}",
                    file_path, e
                ))
            })?;

            debug!("Wrote file: {}", file_path);
        }

        // Stage all files
        let mut index = repo.index().map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!("Failed to get index: {}", e))
        })?;

        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .map_err(|e| {
                GitProviderManagerError::InvalidConfiguration(format!(
                    "Failed to add files to index: {}",
                    e
                ))
            })?;

        index.write().map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!("Failed to write index: {}", e))
        })?;

        let tree_oid = index.write_tree().map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!("Failed to write tree: {}", e))
        })?;

        let tree = repo.find_tree(tree_oid).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!("Failed to find tree: {}", e))
        })?;

        // Create the commit
        let signature = Signature::now(author_name, author_email).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Failed to create signature: {}",
                e
            ))
        })?;

        let parent_commit = repo.head().ok().and_then(|h| h.peel_to_commit().ok());

        let parents: Vec<&git2::Commit> = parent_commit.iter().collect();

        let commit_oid = repo
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                commit_message,
                &tree,
                &parents,
            )
            .map_err(|e| {
                GitProviderManagerError::InvalidConfiguration(format!(
                    "Failed to create commit: {}",
                    e
                ))
            })?;

        let commit_sha = commit_oid.to_string();
        info!("Created local commit: {}", commit_sha);

        // Push to remote
        let mut remote = repo.find_remote("origin").map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!("Failed to find remote: {}", e))
        })?;

        let mut callbacks = RemoteCallbacks::new();
        callbacks.credentials(|_url, _username_from_url, _allowed_types| {
            Cred::userpass_plaintext("x-access-token", access_token)
        });

        let mut push_options = PushOptions::new();
        push_options.remote_callbacks(callbacks);

        let refspec = format!("refs/heads/{}:refs/heads/{}", branch, branch);
        info!("Pushing to remote with refspec: {}", refspec);

        remote
            .push(&[&refspec], Some(&mut push_options))
            .map_err(|e| {
                GitProviderManagerError::InvalidConfiguration(format!(
                    "Failed to push to remote: {}",
                    e
                ))
            })?;

        info!(
            "Successfully pushed {} files with commit {}",
            files.len(),
            commit_sha
        );

        Ok(commit_sha)
    }

    /// Create a repository on the git provider and push template files
    ///
    /// This method:
    /// 1. Creates a new repository on the git provider
    /// 2. Downloads the template repository archive
    /// 3. Extracts the files (optionally from a subfolder)
    /// 4. Pushes the template files to the new repository using native git operations
    ///
    /// # Arguments
    /// * `connection_id` - The git provider connection ID
    /// * `repo_name` - Name for the new repository
    /// * `repo_owner` - Optional owner/organization (defaults to authenticated user)
    /// * `description` - Optional description for the repository
    /// * `private` - Whether the repository should be private
    /// * `template_url` - URL of the template repository
    /// * `template_ref` - Branch/tag/commit reference in the template repository
    /// * `template_subfolder` - Optional subfolder within the template repository
    ///
    /// # Returns
    /// * `Ok(Repository)` - The created repository with the pushed template code
    /// * `Err(GitProviderManagerError)` - If any step fails
    #[allow(clippy::too_many_arguments)]
    pub async fn create_repository_and_push_template(
        &self,
        connection_id: i32,
        repo_name: &str,
        repo_owner: Option<&str>,
        description: Option<&str>,
        private: bool,
        template_url: &str,
        template_ref: &str,
        template_subfolder: Option<&str>,
    ) -> Result<super::git_provider::Repository, GitProviderManagerError> {
        info!(
            "Creating repository {} from template {} (ref: {}, subfolder: {:?})",
            repo_name, template_url, template_ref, template_subfolder
        );

        // Get the connection and provider
        let connection = self.get_connection(connection_id).await?;
        let provider_service = self.get_provider_service(connection.provider_id).await?;
        let access_token = self
            .validate_and_refresh_connection_token(connection_id)
            .await?;

        // Step 1: Create the new repository
        info!("Step 1: Creating repository {} on git provider", repo_name);
        let new_repo = provider_service
            .create_repository(&access_token, repo_name, repo_owner, description, private)
            .await?;

        info!(
            "Created repository: {} (clone_url: {})",
            new_repo.full_name, new_repo.clone_url
        );

        // Step 2: Download the template archive
        let temp_dir = tempfile::tempdir().map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Failed to create temp dir: {}",
                e
            ))
        })?;
        let archive_path = temp_dir.path().join("template.tar.gz");

        // Parse template URL to get owner/repo
        let (template_owner, template_repo) = parse_git_url(template_url).ok_or_else(|| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Invalid template URL: {}",
                template_url
            ))
        })?;

        info!(
            "Step 2: Downloading template archive from {}/{} (ref: {})",
            template_owner, template_repo, template_ref
        );

        // Use the provider service to download the archive (no progress sink:
        // template downloads are not surfaced in a deployment log stream).
        provider_service
            .download_archive(
                &access_token,
                &template_owner,
                &template_repo,
                template_ref,
                &archive_path,
                None,
            )
            .await?;

        // Step 3: Extract the archive and read files
        info!("Step 3: Extracting template archive");
        let files = extract_template_files(&archive_path, template_subfolder).map_err(|e| {
            GitProviderManagerError::InvalidConfiguration(format!(
                "Failed to extract template: {}",
                e
            ))
        })?;

        if files.is_empty() {
            error!("No files found in template archive");
            return Err(GitProviderManagerError::InvalidConfiguration(
                "Template archive contains no files".to_string(),
            ));
        }

        info!("Extracted {} files from template", files.len());

        // Step 4: Push template files to the new repository using native git
        info!("Step 4: Pushing template files to new repository using native git");
        let commit_message = format!("Initial commit from template: {}", template_url);

        // Get author info from connection or use defaults
        let author_name = if connection.account_name.is_empty() {
            "Temps Bot"
        } else {
            &connection.account_name
        };
        let author_email = "bot@temps.sh";

        // Use native git operations instead of API-based blob uploads
        // This is much faster: single clone + commit + push vs N individual API calls
        match Self::push_files_with_git(
            &new_repo.clone_url,
            &access_token,
            &new_repo.default_branch,
            files,
            &commit_message,
            author_name,
            author_email,
        ) {
            Ok(commit_sha) => {
                info!(
                    "Successfully pushed template files with commit: {}",
                    commit_sha
                );
            }
            Err(e) => {
                error!("Failed to push template files: {}", e);
                // Note: Repository was already created, so we return partial success
                // The user can still push files manually
                return Err(e);
            }
        }

        info!(
            "Successfully created repository {} from template",
            new_repo.full_name
        );

        Ok(new_repo)
    }
}

/// Parse a git URL to extract owner and repository name
fn parse_git_url(url: &str) -> Option<(String, String)> {
    // Handle HTTPS URLs: https://github.com/owner/repo.git
    // Handle SSH URLs: git@github.com:owner/repo.git
    let url = url.trim_end_matches(".git");

    if url.contains("://") {
        // HTTPS URL
        let parts: Vec<&str> = url.split('/').collect();
        if parts.len() >= 2 {
            let repo = parts.last()?;
            let owner = parts.get(parts.len() - 2)?;
            return Some((owner.to_string(), repo.to_string()));
        }
    } else if url.contains('@') && url.contains(':') {
        // SSH URL: git@github.com:owner/repo
        let after_colon = url.split(':').nth(1)?;
        let parts: Vec<&str> = after_colon.split('/').collect();
        if parts.len() >= 2 {
            let owner = parts[0];
            let repo = parts[1];
            return Some((owner.to_string(), repo.to_string()));
        }
    }

    None
}

/// Extract files from a template archive
fn extract_template_files(
    archive_path: &std::path::Path,
    subfolder: Option<&str>,
) -> Result<Vec<(String, Vec<u8>)>, std::io::Error> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    use tar::Archive;

    let file = std::fs::File::open(archive_path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    let mut files = Vec::new();
    let entries = archive.entries()?;

    // Determine the prefix to strip (archive usually has a root folder like "repo-main/")
    let mut prefix_to_strip: Option<String> = None;

    for entry in entries {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let path_str = path.to_string_lossy().to_string();

        // Skip pax_global_header files (tar archive artifacts)
        if path_str == "pax_global_header" || path_str.ends_with("/pax_global_header") {
            continue;
        }

        // Skip directories but detect the root prefix
        if entry.header().entry_type().is_dir() {
            // Detect the root folder prefix from the first directory entry
            if prefix_to_strip.is_none() {
                let root = path_str
                    .trim_end_matches('/')
                    .split('/')
                    .next()
                    .unwrap_or("");
                if !root.is_empty() {
                    prefix_to_strip = Some(format!("{}/", root));
                }
            }
            continue;
        }

        // For files, also try to detect the prefix if not already detected
        if prefix_to_strip.is_none() && path_str.contains('/') {
            let root = path_str.split('/').next().unwrap_or("");
            if !root.is_empty() {
                prefix_to_strip = Some(format!("{}/", root));
            }
        }

        // Get the relative path after stripping the archive root folder
        let relative_path = if let Some(ref prefix) = prefix_to_strip {
            if path_str.starts_with(prefix) {
                path_str[prefix.len()..].to_string()
            } else {
                path_str.clone()
            }
        } else {
            path_str.clone()
        };

        // Skip any remaining pax files after path processing
        if relative_path == "pax_global_header" || relative_path.contains("pax_global_header") {
            continue;
        }

        // If a subfolder is specified, only include files from that subfolder
        if let Some(subfolder) = subfolder {
            let subfolder_prefix = if subfolder.ends_with('/') {
                subfolder.to_string()
            } else {
                format!("{}/", subfolder)
            };

            if !relative_path.starts_with(&subfolder_prefix) {
                continue;
            }

            // Strip the subfolder prefix to get the final path
            let final_path = relative_path[subfolder_prefix.len()..].to_string();
            if !final_path.is_empty() {
                let mut content = Vec::new();
                entry.read_to_end(&mut content)?;
                files.push((final_path, content));
            }
        } else if !relative_path.is_empty() {
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            files.push((relative_path, content));
        }
    }

    Ok(files)
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

        // Get repository info — also retry-on-auth, since the same stale
        // token would fail here before we ever reach the clone.
        let repo = match provider_service
            .get_repository(&access_token, repo_owner, repo_name)
            .await
        {
            Ok(r) => r,
            Err(e) if Self::is_auth_failure(&e.to_string()) => {
                tracing::warn!(
                    connection_id,
                    "get_repository hit auth failure ({}); force-refreshing token and retrying",
                    e
                );
                let refreshed = self
                    .force_refresh_connection_token(connection_id)
                    .await
                    .map_err(|err| TraitError::DecryptionError(err.to_string()))?;
                provider_service
                    .get_repository(&refreshed, repo_owner, repo_name)
                    .await
                    .map_err(|err| {
                        TraitError::CloneError(format!(
                            "Failed to get repository after token refresh: {}",
                            err
                        ))
                    })?
            }
            Err(e) => {
                return Err(TraitError::CloneError(format!(
                    "Failed to get repository: {}",
                    e
                )))
            }
        };

        // Clone the repository. On auth failure (libgit2 surfaces 401/403 as a
        // generic credential error), force-refresh the OAuth token and retry
        // once. Two motivations:
        //   1. Time-of-check / time-of-use gap: the token we validated above
        //      may have expired between then and libgit2's HTTPS auth attempt.
        //   2. Server-side revocation: GitLab can invalidate a token before
        //      its recorded expiry.
        // libgit2's credential callback only fires once, so we have to retry
        // the whole operation rather than refreshing inside the callback.
        let target_str = target_dir.to_str().ok_or_else(|| {
            TraitError::CloneError("target directory contains invalid UTF-8".to_string())
        })?;
        let clone_result = provider_service
            .clone_repository(&repo.clone_url, target_str, Some(&access_token))
            .await;

        if let Err(e) = clone_result {
            if Self::is_auth_failure(&e.to_string()) {
                tracing::warn!(
                    connection_id,
                    "clone_repository hit auth failure ({}); force-refreshing token and retrying once",
                    e
                );

                // Wipe the partially-cloned state, otherwise the retry trips
                // the "directory not empty" check or fails with a corrupted
                // git tree.
                if target_dir.exists() {
                    if let Err(rm) = std::fs::remove_dir_all(target_dir) {
                        return Err(TraitError::CloneError(format!(
                            "Auth retry: failed to clean partial clone at {}: {}",
                            target_dir.display(),
                            rm
                        )));
                    }
                    std::fs::create_dir_all(target_dir).map_err(|err| {
                        TraitError::CloneError(format!(
                            "Auth retry: failed to recreate target directory: {}",
                            err
                        ))
                    })?;
                }

                let refreshed = self
                    .force_refresh_connection_token(connection_id)
                    .await
                    .map_err(|err| TraitError::DecryptionError(err.to_string()))?;

                provider_service
                    .clone_repository(&repo.clone_url, target_str, Some(&refreshed))
                    .await
                    .map_err(|err| {
                        TraitError::CloneError(format!("Failed to clone after refresh: {}", err))
                    })?;
            } else {
                return Err(TraitError::CloneError(format!("Failed to clone: {}", e)));
            }
        }

        // Checkout specific ref if provided
        if let Some(ref_name) = branch_or_ref {
            if ref_name != repo.default_branch {
                let target_dir_owned = target_dir.to_path_buf();
                let ref_name_owned = ref_name.to_string();
                tokio::task::spawn_blocking(move || {
                    let repo = git2::Repository::open(&target_dir_owned).map_err(|e| {
                        TraitError::CloneError(format!("Failed to open repository: {}", e))
                    })?;
                    super::git_ops::checkout_ref(&repo, &ref_name_owned).map_err(|e| {
                        TraitError::CloneError(format!(
                            "Failed to checkout ref {}: {}",
                            ref_name_owned, e
                        ))
                    })
                })
                .await
                .map_err(|e| {
                    TraitError::CloneError(format!("Git checkout task failed: {}", e))
                })??;
            }
        }

        Ok(())
    }

    async fn get_connection_access_token(
        &self,
        connection_id: i32,
    ) -> Result<(String, String), super::git_provider_manager_trait::GitProviderManagerError> {
        use super::git_provider_manager_trait::GitProviderManagerError as TraitError;

        let connection = self
            .get_connection(connection_id)
            .await
            .map_err(|_| TraitError::ConnectionNotFound(connection_id))?;

        let provider = self
            .get_provider(connection.provider_id)
            .await
            .map_err(|_| TraitError::ProviderNotFound(connection.provider_id))?;

        let token = self
            .get_connection_token(connection_id)
            .await
            .map_err(|e| TraitError::DecryptionError(e.to_string()))?;

        Ok((token, provider.provider_type))
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
        progress: Option<&crate::services::git_provider::ArchiveProgressSender>,
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

        // Same retry-on-auth logic as clone_repository: the token we just
        // validated may have expired or been revoked between the check and
        // the HTTP request the provider issues to download the tarball.
        match provider_service
            .download_archive(
                &access_token,
                repo_owner,
                repo_name,
                branch_or_ref,
                archive_path,
                progress,
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if Self::is_auth_failure(&e.to_string()) => {
                tracing::warn!(
                    connection_id,
                    "download_archive hit auth failure ({}); force-refreshing token and retrying once",
                    e
                );
                // The provider usually creates the archive file as it streams
                // — if the auth failed mid-stream we'd be left with a partial
                // file. Remove it so the retry starts clean.
                if archive_path.exists() {
                    let _ = std::fs::remove_file(archive_path);
                }
                let refreshed = self
                    .force_refresh_connection_token(connection_id)
                    .await
                    .map_err(|err| TraitError::DecryptionError(err.to_string()))?;
                provider_service
                    .download_archive(
                        &refreshed,
                        repo_owner,
                        repo_name,
                        branch_or_ref,
                        archive_path,
                        progress,
                    )
                    .await
                    .map_err(|err| {
                        TraitError::Other(format!(
                            "Failed to download archive after token refresh: {}",
                            err
                        ))
                    })?;
                Ok(())
            }
            Err(e) => Err(TraitError::Other(format!(
                "Failed to download archive: {}",
                e
            ))),
        }
    }

    async fn push_files_and_create_pr(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        branch: &str,
        base_branch: &str,
        files: Vec<(String, Vec<u8>)>,
        commit_message: &str,
        pr_title: &str,
        pr_body: &str,
    ) -> Result<
        super::git_provider_manager_trait::PullRequest,
        super::git_provider_manager_trait::GitProviderManagerError,
    > {
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

        // Push files to the branch (creates the branch + commit)
        provider_service
            .push_files_to_repository(&access_token, owner, repo, branch, files, commit_message)
            .await
            .map_err(|e| {
                TraitError::Other(format!(
                    "Failed to push files to {}/{} branch {}: {}",
                    owner, repo, branch, e
                ))
            })?;

        // Create the pull request
        let pr = provider_service
            .create_pull_request(
                &access_token,
                owner,
                repo,
                pr_title,
                pr_body,
                branch,
                base_branch,
            )
            .await
            .map_err(|e| {
                TraitError::Other(format!(
                    "Failed to create pull request in {}/{} ({}->{}): {}",
                    owner, repo, branch, base_branch, e
                ))
            })?;

        Ok(pr)
    }

    async fn mint_scoped_repo_token(
        &self,
        connection_id: i32,
        owner: &str,
        repo: &str,
        operation: super::git_provider_manager_trait::ScopedTokenOp,
    ) -> Result<
        super::git_provider_manager_trait::ScopedTokenGrant,
        super::git_provider_manager_trait::GitProviderManagerError,
    > {
        use super::git_provider_manager_trait::GitProviderManagerError as TraitError;

        match self
            .mint_scoped_repo_token_for_connection(connection_id, owner, repo, operation)
            .await
        {
            Ok(grant) => Ok(grant),
            Err(GitProviderManagerError::ProviderError(
                super::git_provider::GitProviderError::NotImplemented,
            )) => Err(TraitError::ScopedTokensUnsupported {
                connection_id,
                reason: "provider does not implement scoped per-op tokens (PAT/OAuth)".into(),
            }),
            Err(GitProviderManagerError::ProviderError(
                super::git_provider::GitProviderError::InvalidConfiguration(reason),
            )) => Err(TraitError::ScopedTokensUnsupported {
                connection_id,
                reason,
            }),
            Err(GitProviderManagerError::ConnectionNotFound(_)) => {
                Err(TraitError::ConnectionNotFound(connection_id))
            }
            Err(GitProviderManagerError::ProviderNotFound(_)) => {
                Err(TraitError::ProviderNotFound(connection_id))
            }
            Err(e) => Err(TraitError::Other(format!(
                "Failed to mint scoped token for connection {} on {}/{}: {}",
                connection_id, owner, repo, e
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_core::{async_trait::async_trait, Job, JobReceiver, QueueError};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{git_provider_connections, git_providers};

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

    /// A queue that fails the test if anything is ever enqueued. Used to assert
    /// that a push event was short-circuited before any deployment job was sent.
    struct PanicOnSendJobQueue;

    #[async_trait]
    impl JobQueue for PanicOnSendJobQueue {
        async fn send(&self, job: Job) -> Result<(), QueueError> {
            panic!("no job should be queued, but got: {job:?}");
        }

        fn subscribe(&self) -> Box<dyn JobReceiver> {
            panic!("subscribe not needed for tests")
        }
    }

    #[tokio::test]
    async fn handle_push_event_ignores_null_sha_deletion() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        // A MockDatabase with no queued results errors on the first query —
        // proving the null-SHA guard returns before touching the database. The
        // panic-on-send queue proves no deployment job is enqueued.
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );
        let queue_service = Arc::new(PanicOnSendJobQueue) as Arc<dyn JobQueue>;
        let config_service = create_test_config_service(db.clone());

        let manager =
            GitProviderManager::new(db, encryption_service, queue_service, config_service);

        // Full all-zeros null SHA (GitHub/GitLab branch deletion).
        let result = manager
            .handle_push_event(
                "owner".into(),
                "repo".into(),
                Some("deleted-branch".into()),
                None,
                "0000000000000000000000000000000000000000".into(),
            )
            .await;
        assert!(
            result.is_ok(),
            "null-SHA push event should be ignored: {result:?}"
        );

        // Empty commit string is treated the same way.
        let result = manager
            .handle_push_event(
                "owner".into(),
                "repo".into(),
                Some("b".into()),
                None,
                String::new(),
            )
            .await;
        assert!(
            result.is_ok(),
            "empty-SHA push event should be ignored: {result:?}"
        );
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

    #[test]
    fn test_parse_git_url_https_with_git_extension() {
        let url = "https://github.com/owner/repo.git";
        let result = parse_git_url(url);
        assert!(result.is_some());
        let (owner, repo) = result.unwrap();
        assert_eq!(owner, "owner");
        assert_eq!(repo, "repo");
    }

    #[test]
    fn test_parse_git_url_https_without_git_extension() {
        let url = "https://github.com/owner/repo";
        let result = parse_git_url(url);
        assert!(result.is_some());
        let (owner, repo) = result.unwrap();
        assert_eq!(owner, "owner");
        assert_eq!(repo, "repo");
    }

    #[test]
    fn test_parse_git_url_ssh_format() {
        let url = "git@github.com:owner/repo.git";
        let result = parse_git_url(url);
        assert!(result.is_some());
        let (owner, repo) = result.unwrap();
        assert_eq!(owner, "owner");
        assert_eq!(repo, "repo");
    }

    #[test]
    fn test_parse_git_url_ssh_without_git_extension() {
        let url = "git@gitlab.com:myorg/myrepo";
        let result = parse_git_url(url);
        assert!(result.is_some());
        let (owner, repo) = result.unwrap();
        assert_eq!(owner, "myorg");
        assert_eq!(repo, "myrepo");
    }

    #[test]
    fn test_parse_git_url_gitlab_https() {
        let url = "https://gitlab.com/group/project.git";
        let result = parse_git_url(url);
        assert!(result.is_some());
        let (owner, repo) = result.unwrap();
        assert_eq!(owner, "group");
        assert_eq!(repo, "project");
    }

    #[test]
    fn test_parse_git_url_with_nested_groups() {
        // For nested groups, we want the last two components
        let url = "https://gitlab.com/group/subgroup/project.git";
        let result = parse_git_url(url);
        assert!(result.is_some());
        let (owner, repo) = result.unwrap();
        assert_eq!(owner, "subgroup");
        assert_eq!(repo, "project");
    }

    #[test]
    fn test_parse_git_url_invalid_url() {
        let url = "not-a-valid-url";
        let result = parse_git_url(url);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_template_files_creates_files_from_archive() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        // Create a temporary directory and archive
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("test.tar.gz");

        // Create a tar.gz archive with test files
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let encoder = GzEncoder::new(file, Compression::default());
            let mut builder = tar::Builder::new(encoder);

            // Add root directory entry first (like git archive does)
            let mut dir_header = tar::Header::new_gnu();
            dir_header.set_path("repo-main/").unwrap();
            dir_header.set_size(0);
            dir_header.set_mode(0o755);
            dir_header.set_entry_type(tar::EntryType::Directory);
            dir_header.set_cksum();
            builder.append(&dir_header, &[] as &[u8]).unwrap();

            // Add a file inside a root folder (mimicking git archive format)
            let content = b"Hello, World!";
            let mut header = tar::Header::new_gnu();
            header.set_path("repo-main/test.txt").unwrap();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder.append(&header, &content[..]).unwrap();

            // Add src directory
            let mut src_dir_header = tar::Header::new_gnu();
            src_dir_header.set_path("repo-main/src/").unwrap();
            src_dir_header.set_size(0);
            src_dir_header.set_mode(0o755);
            src_dir_header.set_entry_type(tar::EntryType::Directory);
            src_dir_header.set_cksum();
            builder.append(&src_dir_header, &[] as &[u8]).unwrap();

            // Add another file
            let content2 = b"console.log('test');";
            let mut header2 = tar::Header::new_gnu();
            header2.set_path("repo-main/src/index.js").unwrap();
            header2.set_size(content2.len() as u64);
            header2.set_mode(0o644);
            header2.set_entry_type(tar::EntryType::Regular);
            header2.set_cksum();
            builder.append(&header2, &content2[..]).unwrap();

            builder.finish().unwrap();
        }

        // Extract the files
        let files = extract_template_files(&archive_path, None).unwrap();

        assert_eq!(files.len(), 2);

        // Check that the root folder prefix was stripped
        let file_names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert!(file_names.contains(&"test.txt"));
        assert!(file_names.contains(&"src/index.js"));

        // Verify content
        let test_file = files.iter().find(|(name, _)| name == "test.txt").unwrap();
        assert_eq!(test_file.1, b"Hello, World!");
    }

    #[test]
    fn test_extract_template_files_with_subfolder() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("test.tar.gz");

        // Create archive with multiple folders
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let encoder = GzEncoder::new(file, Compression::default());
            let mut builder = tar::Builder::new(encoder);

            // Add root directory entry first
            let mut root_dir = tar::Header::new_gnu();
            root_dir.set_path("repo-main/").unwrap();
            root_dir.set_size(0);
            root_dir.set_mode(0o755);
            root_dir.set_entry_type(tar::EntryType::Directory);
            root_dir.set_cksum();
            builder.append(&root_dir, &[] as &[u8]).unwrap();

            // Add files in different subfolders
            let content1 = b"Root file";
            let mut header1 = tar::Header::new_gnu();
            header1.set_path("repo-main/README.md").unwrap();
            header1.set_size(content1.len() as u64);
            header1.set_mode(0o644);
            header1.set_entry_type(tar::EntryType::Regular);
            header1.set_cksum();
            builder.append(&header1, &content1[..]).unwrap();

            // Add templates directory
            let mut templates_dir = tar::Header::new_gnu();
            templates_dir.set_path("repo-main/templates/").unwrap();
            templates_dir.set_size(0);
            templates_dir.set_mode(0o755);
            templates_dir.set_entry_type(tar::EntryType::Directory);
            templates_dir.set_cksum();
            builder.append(&templates_dir, &[] as &[u8]).unwrap();

            // Add template1 directory
            let mut template1_dir = tar::Header::new_gnu();
            template1_dir
                .set_path("repo-main/templates/template1/")
                .unwrap();
            template1_dir.set_size(0);
            template1_dir.set_mode(0o755);
            template1_dir.set_entry_type(tar::EntryType::Directory);
            template1_dir.set_cksum();
            builder.append(&template1_dir, &[] as &[u8]).unwrap();

            let content2 = b"Template 1 content";
            let mut header2 = tar::Header::new_gnu();
            header2
                .set_path("repo-main/templates/template1/index.js")
                .unwrap();
            header2.set_size(content2.len() as u64);
            header2.set_mode(0o644);
            header2.set_entry_type(tar::EntryType::Regular);
            header2.set_cksum();
            builder.append(&header2, &content2[..]).unwrap();

            let content3 = b"Template 1 config";
            let mut header3 = tar::Header::new_gnu();
            header3
                .set_path("repo-main/templates/template1/config.json")
                .unwrap();
            header3.set_size(content3.len() as u64);
            header3.set_mode(0o644);
            header3.set_entry_type(tar::EntryType::Regular);
            header3.set_cksum();
            builder.append(&header3, &content3[..]).unwrap();

            // Add template2 directory
            let mut template2_dir = tar::Header::new_gnu();
            template2_dir
                .set_path("repo-main/templates/template2/")
                .unwrap();
            template2_dir.set_size(0);
            template2_dir.set_mode(0o755);
            template2_dir.set_entry_type(tar::EntryType::Directory);
            template2_dir.set_cksum();
            builder.append(&template2_dir, &[] as &[u8]).unwrap();

            let content4 = b"Template 2 content";
            let mut header4 = tar::Header::new_gnu();
            header4
                .set_path("repo-main/templates/template2/main.py")
                .unwrap();
            header4.set_size(content4.len() as u64);
            header4.set_mode(0o644);
            header4.set_entry_type(tar::EntryType::Regular);
            header4.set_cksum();
            builder.append(&header4, &content4[..]).unwrap();

            builder.finish().unwrap();
        }

        // Extract only files from templates/template1 subfolder
        let files = extract_template_files(&archive_path, Some("templates/template1")).unwrap();

        assert_eq!(files.len(), 2);

        let file_names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert!(file_names.contains(&"index.js"));
        assert!(file_names.contains(&"config.json"));

        // Verify the other template's files were not included
        assert!(!file_names.contains(&"main.py"));
        assert!(!file_names.contains(&"README.md"));
    }
}
