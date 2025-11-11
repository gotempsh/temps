use chrono::Utc;
use hex;
use hmac::{Hmac, Mac};
use octocrab::models::{AppId, InstallationId, InstallationRepositories, InstallationToken};
use octocrab::params::apps::CreateInstallationAccessToken;
use octocrab::Octocrab;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::sync::Arc;
use temps_core::UtcDateTime;
use temps_database::DbConnection;
use temps_entities::{self, repositories};
use temps_entities::{git_provider_connections, git_providers, projects};
use thiserror::Error;
use tracing::{debug, error, info, warn};
use url::Url;

#[derive(Error, Debug)]
pub enum GithubAppServiceError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),
    #[error("GitHub API error: {0}")]
    GithubApiError(String),
    #[error("App not found: {0}")]
    NotFound(String),
    #[error("Failed to decrypt access token: {0}")]
    DecryptionFailed(String),
    #[error("Failed to encrypt access token: {0}")]
    EncryptionFailed(String),
    #[error("Failed to create private key: {0}")]
    PrivateKeyCreationFailed(String),
    #[error("Conflict: {0}")]
    Conflict(String),
    #[error("Installation not found")]
    InstallationNotFound,
    #[error("Invalid webhook signature")]
    InvalidWebhookSignature,
    #[error("Unauthorized: {0}")]
    Unauthorized(String),
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),
    #[error("Validation error: {0}")]
    Validation(String),
    #[error("Other")]
    Other(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Repository {
    pub id: u64,
    pub name: String,
    pub full_name: String,
    pub owner: String,
    pub private: bool,
    pub framework: Option<String>,
    pub description: Option<String>,
    pub package_manager: Option<String>,
    pub default_branch: String,
    pub installation_id: i32,
    pub git_provider_connection_id: Option<i32>,
    pub pushed_at: Option<UtcDateTime>,
    pub stargazers_count: i32,
    pub watchers_count: i32,
    pub language: String,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RepositoryFramework {
    pub framework: Framework,
    pub package_manager: PackageManager,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageManager {
    Bun,
    Npm,
    Yarn,
    Unknown,
}

impl std::fmt::Display for PackageManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageManager::Bun => write!(f, "bun"),
            PackageManager::Npm => write!(f, "npm"),
            PackageManager::Yarn => write!(f, "yarn"),
            PackageManager::Unknown => write!(f, "Unknown"),
        }
    }
}

impl TryFrom<&str> for PackageManager {
    type Error = PackageManager;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "bun" => Ok(PackageManager::Bun),
            "npm" => Ok(PackageManager::Npm),
            "yarn" => Ok(PackageManager::Yarn),
            _ => Err(PackageManager::Unknown),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Framework {
    NextJs,
    Vite,
    Rsbuild,
    CreateReactApp,
    Docusaurus, // Add Docusaurus variant
    Dockerfile, // Add Dockerfile variant
    Unknown,
}

impl std::fmt::Display for Framework {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Framework::NextJs => write!(f, "Next.js"),
            Framework::Vite => write!(f, "Vite"),
            Framework::CreateReactApp => write!(f, "Create React App"),
            Framework::Unknown => write!(f, "Unknown"),
            Framework::Rsbuild => write!(f, "Rsbuild"),
            Framework::Docusaurus => write!(f, "Docusaurus"), // Add Docusaurus string conversion
            Framework::Dockerfile => write!(f, "Dockerfile"), // Add Dockerfile string conversion
        }
    }
}

impl TryFrom<&str> for Framework {
    type Error = Framework;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "Next.js" => Ok(Framework::NextJs),
            "Vite" => Ok(Framework::Vite),
            "Create React App" => Ok(Framework::CreateReactApp),
            "Unknown" => Ok(Framework::Unknown),
            "Rsbuild" => Ok(Framework::Rsbuild),
            "Docusaurus" => Ok(Framework::Docusaurus), // Add Docusaurus conversion
            "Dockerfile" => Ok(Framework::Dockerfile), // Add Dockerfile conversion
            _ => Err(Framework::Unknown),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetResponse {
    pub directories: Vec<DirectoryPreset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryPreset {
    pub path: String,
    pub framework: FrameworkPreset,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameworkPreset {
    pub slug: String,
    pub label: String,
    pub project_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchInfo {
    pub name: String,
    pub commit: CommitRef,
    pub protected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitRef {
    pub sha: String,
    pub url: String,
}

#[derive(Clone)]
pub struct GithubAppService {
    db: Arc<DbConnection>,
    queue_service: Arc<dyn temps_core::jobs::JobQueue>,
    git_provider_manager: Arc<crate::services::git_provider_manager::GitProviderManager>,
}

// Helper struct to represent GitHub App data from git_providers
#[derive(Debug, Clone)]
pub struct GitHubAppData {
    pub provider_id: i32,
    pub id: i32, // For backward compatibility, set to provider_id
    pub app_id: i32,
    pub name: String,
    pub slug: String,
    pub client_id: String,
    pub client_secret: String,
    pub private_key: String,
    pub webhook_secret: String,
    pub url: String,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
}

// Helper struct to represent GitHub installation data from git_provider_connections
#[derive(Debug, Clone)]
pub struct GitHubInstallationData {
    pub connection_id: i32,
    pub provider_id: i32,
    pub id: i32, // For backward compatibility, set to connection_id
    pub installation_id: i32,
    pub github_app_id: i32, // For backward compatibility, set to provider_id
    pub account_name: String,
    pub account_type: String,
    pub account_id: i32, // Default to 0 if not available
    pub access_token: Option<String>,
    pub token_expires_at: Option<UtcDateTime>,
    pub last_synced_at: Option<UtcDateTime>,
    pub html_url: Option<String>,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub suspended_at: Option<UtcDateTime>,
    pub suspended_by: Option<String>,
}

impl GithubAppService {
    pub fn new(
        db: Arc<DbConnection>,
        queue_service: Arc<dyn temps_core::jobs::JobQueue>,
        git_provider_manager: Arc<super::git_provider_manager::GitProviderManager>,
    ) -> Self {
        Self {
            db,
            queue_service,
            git_provider_manager,
        }
    }
    // Helper method to extract GitHub App data from a git_provider
    async fn extract_github_app_data(
        &self,
        provider: &git_providers::Model,
    ) -> Result<GitHubAppData, GithubAppServiceError> {
        let auth_config = self
            .git_provider_manager
            .decrypt_sensitive_data(&provider.auth_config)
            .await
            .map_err(|e| GithubAppServiceError::DecryptionFailed(e.to_string()))?;

        match serde_json::from_value::<crate::services::git_provider::AuthMethod>(auth_config) {
            Ok(crate::services::git_provider::AuthMethod::GitHubApp {
                app_id,
                client_id,
                client_secret,
                private_key,
                webhook_secret,
            }) => {
                // Extract slug from auth_config or derive from name
                let decrypted_config = self
                    .git_provider_manager
                    .decrypt_sensitive_data(&provider.auth_config)
                    .await
                    .map_err(|e| GithubAppServiceError::DecryptionFailed(e.to_string()))?;

                let slug = decrypted_config
                    .get("slug")
                    .and_then(|s| s.as_str())
                    .unwrap_or(&provider.name)
                    .to_string();

                Ok(GitHubAppData {
                    provider_id: provider.id,
                    id: provider.id, // For backward compatibility
                    app_id,
                    name: provider.name.clone(),
                    slug,
                    client_id,
                    client_secret,
                    private_key,
                    webhook_secret,
                    url: provider
                        .base_url
                        .clone()
                        .unwrap_or_else(|| "https://github.com".to_string()),
                    created_at: provider.created_at,
                    updated_at: provider.updated_at,
                })
            }
            _ => Err(GithubAppServiceError::NotFound(
                "Provider is not a GitHub App".to_string(),
            )),
        }
    }

    pub async fn get_github_app_client_by_app_id(
        &self,
        app_id: i32,
    ) -> Result<(Octocrab, GitHubAppData), GithubAppServiceError> {
        // Get the provider for this GitHub App
        let provider = self
            .git_provider_manager
            .get_github_app_provider_by_app_id(app_id)
            .await
            .map_err(|e| {
                GithubAppServiceError::NotFound(format!(
                    "GitHub App with app_id {} not found: {}",
                    app_id, e
                ))
            })?;

        let app_data = self.extract_github_app_data(&provider).await?;

        // Create JWT for authentication
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(app_data.private_key.as_bytes())
            .map_err(|e| GithubAppServiceError::PrivateKeyCreationFailed(e.to_string()))?;

        let app_id_param = AppId(app_id as u64);
        let token = octocrab::auth::create_jwt(app_id_param, &key)
            .map_err(|e| GithubAppServiceError::GithubApiError(e.to_string()))?;
        let octocrab = Octocrab::builder()
            .personal_token(token)
            .build()
            .map_err(|e| GithubAppServiceError::GithubApiError(e.to_string()))?;
        Ok((octocrab, app_data))
    }

    pub async fn get_installation_token_client(
        &self,
        git_provider_connection_id: i32,
    ) -> Result<(Octocrab, GitHubAppData, String), GithubAppServiceError> {
        // Get the connection directly
        let connection = self
            .git_provider_manager
            .get_connection(git_provider_connection_id)
            .await
            .map_err(|e| {
                GithubAppServiceError::NotFound(format!(
                    "Git provider connection with id {} not found: {}",
                    git_provider_connection_id, e
                ))
            })?;

        // Get the provider for this connection
        let provider = self
            .git_provider_manager
            .get_provider(connection.provider_id)
            .await
            .map_err(|e| {
                GithubAppServiceError::NotFound(format!(
                    "GitHub App provider {} not found: {}",
                    connection.provider_id, e
                ))
            })?;

        let app_data = self.extract_github_app_data(&provider).await?;

        // Check if we have a valid access token in the connection
        if let Some(token) = connection.access_token {
            if let Some(expires_at) = connection.token_expires_at {
                if expires_at > chrono::Utc::now() {
                    // Token is still valid, decrypt and use it
                    let access_token = self
                        .git_provider_manager
                        .decrypt_sensitive_data(&serde_json::json!(token))
                        .await
                        .map_err(|e| GithubAppServiceError::DecryptionFailed(e.to_string()))?;

                    let access_token_str = access_token
                        .as_str()
                        .ok_or_else(|| {
                            GithubAppServiceError::DecryptionFailed(
                                "Invalid token format".to_string(),
                            )
                        })?
                        .to_string();

                    let octocrab = Octocrab::builder()
                        .personal_token(access_token_str.clone())
                        .build()
                        .map_err(|e| GithubAppServiceError::GithubApiError(e.to_string()))?;
                    return Ok((octocrab, app_data, access_token_str));
                }
            }
        }

        // Need to create a new access token
        // Extract installation_id from the connection's external_id
        let installation_id = connection
            .installation_id
            .as_ref()
            .and_then(|id| id.parse::<i32>().ok())
            .ok_or_else(|| {
                GithubAppServiceError::InvalidConfiguration(
                    "Connection missing valid installation ID in external_id".to_string(),
                )
            })?;

        let key = jsonwebtoken::EncodingKey::from_rsa_pem(app_data.private_key.as_bytes())
            .map_err(|e| GithubAppServiceError::PrivateKeyCreationFailed(e.to_string()))?;

        let app_id_param = AppId(app_data.app_id as u64);
        let token = octocrab::auth::create_jwt(app_id_param, &key)
            .map_err(|e| GithubAppServiceError::GithubApiError(e.to_string()))?;
        let octocrab = Octocrab::builder()
            .personal_token(token)
            .build()
            .map_err(|e| GithubAppServiceError::GithubApiError(e.to_string()))?;

        let installation = octocrab
            .apps()
            .installation(InstallationId(installation_id as u64))
            .await
            .map_err(|e| GithubAppServiceError::GithubApiError(e.to_string()))?;

        let create_access_token = CreateInstallationAccessToken::default();
        let gh_access_tokens_url = Url::parse(installation.access_tokens_url.as_ref().unwrap())
            .map_err(|e| GithubAppServiceError::GithubApiError(e.to_string()))?;

        let access: InstallationToken = octocrab
            .post(gh_access_tokens_url.path(), Some(&create_access_token))
            .await
            .map_err(|e| GithubAppServiceError::GithubApiError(e.to_string()))?;

        // Update the access token in the connection
        let expires_at = chrono::DateTime::parse_from_rfc3339(&access.expires_at.unwrap())
            .map_err(|e| GithubAppServiceError::GithubApiError(e.to_string()))?;
        let expires_at = expires_at.with_timezone(&chrono::Utc);

        // Update the connection with the new token
        self.git_provider_manager
            .update_connection_tokens(connection.id, access.token.clone(), None, Some(expires_at))
            .await
            .map_err(|e| {
                GithubAppServiceError::Other(format!("Failed to update connection tokens: {}", e))
            })?;

        let octocrab = octocrab::OctocrabBuilder::new()
            .personal_token(access.token.clone())
            .build()
            .unwrap();

        Ok((octocrab, app_data, access.token))
    }
    pub async fn update_last_synced_at(
        &self,
        installation_id_p: i32,
    ) -> Result<(), GithubAppServiceError> {
        info!(
            "Updating last_synced_at for installation id: {}",
            installation_id_p
        );

        // Get the connection for this installation
        let connection = self
            .git_provider_manager
            .get_connection_by_installation_id(&installation_id_p.to_string())
            .await
            .map_err(|_| GithubAppServiceError::InstallationNotFound)?;

        // Update the connection's last_synced_at
        let mut active_connection: git_provider_connections::ActiveModel = connection.into();
        active_connection.last_synced_at = Set(Some(chrono::Utc::now()));
        active_connection.update(self.db.as_ref()).await?;

        info!(
            "Successfully updated last_synced_at for installation id: {}",
            installation_id_p
        );
        Ok(())
    }
    pub async fn sync_repository(
        &self,
        repo_owner: &str,
        repo_name: &str,
        github_app_id: i32,
        installation_id: i32,
        git_provider_connection_id: Option<i32>,
    ) -> Result<(), GithubAppServiceError> {
        info!("Syncing repository: {}/{}", repo_owner, repo_name);

        // Fetch the repository from GitHub API
        // If we have a git_provider_connection_id, use it; otherwise try to find one for this installation
        let connection_id = if let Some(id) = git_provider_connection_id {
            id
        } else {
            // Try to get connection by installation_id
            self.git_provider_manager
                .get_connection_by_installation_id(&installation_id.to_string())
                .await
                .map(|conn| conn.id)
                .map_err(|e| {
                    GithubAppServiceError::NotFound(format!(
                        "No connection found for installation {}: {}",
                        installation_id, e
                    ))
                })?
        };

        let (octocrab, _, _) = self.get_installation_token_client(connection_id).await?;

        let repo = octocrab
            .repos(repo_owner, repo_name)
            .get()
            .await
            .map_err(|e| GithubAppServiceError::GithubApiError(e.to_string()))?;

        match self
            .store_repository(&repo, github_app_id, installation_id)
            .await
        {
            Ok(_) => {
                info!(
                    "Successfully synced repository: {}/{}",
                    repo_owner, repo_name
                );
                Ok(())
            }
            Err(e) => {
                error!(
                    "Failed to store repository: {}/{}: {}",
                    repo_owner, repo_name, e
                );
                Err(e)
            }
        }
    }
    /// Store a repository from webhook payload (simplified version with limited fields)
    pub async fn store_repository_from_webhook(
        &self,
        repo: &octocrab::models::webhook_events::InstallationEventRepository,
        installation_id_p: i32,
    ) -> Result<(), GithubAppServiceError> {
        info!(
            "Storing repository from webhook payload: {}",
            repo.full_name
        );

        // Get the git provider connection for this installation
        let connection_id = self
            .git_provider_manager
            .get_connection_by_installation_id(&installation_id_p.to_string())
            .await
            .map(|conn| conn.id)
            .map_err(|e| {
                GithubAppServiceError::NotFound(format!(
                    "No connection found for installation {}: {}",
                    installation_id_p, e
                ))
            })?;

        let repo_owner = repo.full_name.split('/').next().unwrap_or("");

        // Check if the repository already exists in the database
        let existing_repo = repositories::Entity::find()
            .filter(repositories::Column::FullName.eq(&repo.full_name))
            .one(self.db.as_ref())
            .await?;

        let repo_model = repositories::ActiveModel {
            id: if let Some(existing) = existing_repo {
                Set(existing.id)
            } else {
                sea_orm::NotSet
            },
            git_provider_connection_id: Set(connection_id),
            clone_url: Set(None),
            ssh_url: Set(None),
            owner: Set(repo_owner.to_string()),
            name: Set(repo.name.clone()),
            full_name: Set(repo.full_name.clone()),
            description: Set(None),
            private: Set(repo.private),
            fork: Set(false),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            pushed_at: Set(chrono::Utc::now()),
            size: Set(0),
            stargazers_count: Set(0),
            watchers_count: Set(0),
            language: Set(None),
            default_branch: Set("main".to_string()),
            open_issues_count: Set(0),
            topics: Set(String::new()),
            repo_object: Set(String::new()),
            installation_id: Set(Some(installation_id_p)),
            preset: Set(None),
        };

        let saved_repo = repo_model.save(self.db.as_ref()).await?;

        // Queue framework detection job
        if let Err(e) = self
            .queue_service
            .send(temps_core::jobs::Job::UpdateRepoFramework(
                temps_core::jobs::UpdateRepoFrameworkJob {
                    repo_id: saved_repo.id.unwrap(),
                },
            ))
            .await
        {
            warn!(
                "Failed to queue framework detection for repository {}: {}",
                repo.full_name, e
            );
        }

        info!(
            "Successfully stored repository from webhook payload: {} (framework detection queued)",
            repo.full_name
        );
        Ok(())
    }

    pub async fn store_repository(
        &self,
        repo: &octocrab::models::Repository,
        _github_app_id_p: i32,
        installation_id_p: i32,
    ) -> Result<(), GithubAppServiceError> {
        info!(
            "Storing repository: {}",
            repo.full_name.as_deref().unwrap_or("Unknown")
        );

        let repo_owner = repo.owner.as_ref().map(|o| o.login.as_str()).unwrap_or("");
        let full_name_val = repo.full_name.clone().unwrap_or_default();

        // Get the git provider connection for this installation
        let git_provider_connection_id = self
            .git_provider_manager
            .get_connection_by_installation_id(&installation_id_p.to_string())
            .await
            .map(|conn| conn.id)
            .map_err(|e| {
                GithubAppServiceError::NotFound(format!(
                    "No connection found for installation {}: {}",
                    installation_id_p, e
                ))
            })?;

        // Check if the repository already exists in the database
        let existing_repo = repositories::Entity::find()
            .filter(repositories::Column::Owner.eq(repo_owner))
            .filter(repositories::Column::Name.eq(&repo.name))
            .one(self.db.as_ref())
            .await?;

        let repo_language = repo
            .language
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let topics_p = repo
            .topics
            .as_ref()
            .map(|t| serde_json::to_string(t).unwrap_or_default());

        let repo_model = repositories::ActiveModel {
            id: if let Some(existing) = existing_repo {
                Set(existing.id)
            } else {
                sea_orm::NotSet
            },
            git_provider_connection_id: Set(git_provider_connection_id),
            clone_url: Set(repo.clone_url.as_ref().map(|url| url.to_string())),
            ssh_url: Set(repo.ssh_url.as_ref().map(|url| url.to_string())),
            owner: Set(repo_owner.to_string()),
            name: Set(repo.name.clone()),
            full_name: Set(full_name_val),
            description: Set(repo.description.clone()),
            private: Set(repo.private.unwrap_or(false)),
            fork: Set(repo.fork.unwrap_or(false)),
            created_at: Set(repo.created_at.unwrap_or(Utc::now())),
            updated_at: Set(repo.updated_at.unwrap_or(Utc::now())),
            pushed_at: Set(repo.pushed_at.unwrap_or(Utc::now())),
            size: Set(repo.size.unwrap_or(0) as i32),
            stargazers_count: Set(repo.stargazers_count.unwrap_or(0) as i32),
            watchers_count: Set(repo.watchers_count.unwrap_or(0) as i32),
            language: Set(Some(repo_language.to_string())),
            default_branch: Set(repo
                .default_branch
                .clone()
                .unwrap_or_else(|| "main".to_string())),
            open_issues_count: Set(repo.open_issues_count.unwrap_or(0) as i32),
            topics: Set(topics_p.unwrap_or_default()),
            repo_object: Set(serde_json::to_string(&repo).unwrap_or_default()),
            installation_id: Set(Some(installation_id_p)),
            preset: Set(None),
        };

        let saved_repo = repo_model.save(self.db.as_ref()).await?;

        // Queue framework detection job
        if let Err(e) = self
            .queue_service
            .send(temps_core::jobs::Job::UpdateRepoFramework(
                temps_core::jobs::UpdateRepoFrameworkJob {
                    repo_id: saved_repo.id.unwrap(),
                },
            ))
            .await
        {
            warn!(
                "Failed to queue framework detection for repository {}: {}",
                repo.full_name.as_deref().unwrap_or("Unknown"),
                e
            );
        }

        info!(
            "Successfully stored repository: {} (framework detection queued)",
            repo.full_name.as_deref().unwrap_or("Unknown")
        );
        Ok(())
    }
    pub async fn sync_repositories(
        &self,
        installation_id_p: i32,
        git_provider_connection_id: Option<i32>,
    ) -> Result<(), GithubAppServiceError> {
        info!("Synchronizing repositories from GitHub installation with the database");

        let (_, repos) = self
            .get_repositories_for_installation(installation_id_p, git_provider_connection_id)
            .await?;

        // Get the git provider connection for this installation
        let connection_id = if let Some(id) = git_provider_connection_id {
            id
        } else {
            self.git_provider_manager
                .get_connection_by_installation_id(&installation_id_p.to_string())
                .await
                .map(|conn| conn.id)
                .map_err(|e| {
                    GithubAppServiceError::NotFound(format!(
                        "No connection found for installation {}: {}",
                        installation_id_p, e
                    ))
                })?
        };

        // Use SeaORM transaction
        let txn = self.db.begin().await?;

        // Process repositories in batches using upsert
        let repos_count = repos.len();
        info!(
            "Processing {} repositories for installation {}",
            repos_count, installation_id_p
        );

        for repo in repos {
            let repo_owner = repo.full_name.split('/').next().unwrap_or("").to_string();

            let repo_model = repositories::ActiveModel {
                id: sea_orm::NotSet,
                git_provider_connection_id: Set(connection_id),
                clone_url: Set(None),
                ssh_url: Set(None),
                owner: Set(repo_owner.clone()),
                name: Set(repo.name.clone()),
                full_name: Set(repo.full_name.clone()),
                description: Set(None),
                private: Set(repo.private),
                fork: Set(false),
                created_at: Set(repo.created_at),
                updated_at: Set(repo.updated_at),
                pushed_at: Set(repo.pushed_at.unwrap_or_else(chrono::Utc::now)),
                size: Set(0),
                stargazers_count: Set(repo.stargazers_count),
                watchers_count: Set(repo.watchers_count),
                language: Set(Some(repo.language.clone())),
                default_branch: Set(repo.default_branch.clone()),
                open_issues_count: Set(0),
                topics: Set(String::new()),
                repo_object: Set(String::new()),
                installation_id: Set(Some(installation_id_p)),
                preset: Set(None),
            };

            // Try to find existing repository
            let existing_repo = repositories::Entity::find()
                .filter(repositories::Column::FullName.eq(&repo.full_name))
                .one(&txn)
                .await?;

            if let Some(existing) = existing_repo {
                // Update existing repository
                debug!("Updating existing repository: {}", repo.full_name);
                let mut active_repo: repositories::ActiveModel = existing.into();
                active_repo.git_provider_connection_id = Set(connection_id);
                active_repo.owner = Set(repo_owner);
                active_repo.name = Set(repo.name.clone());
                active_repo.installation_id = Set(Some(installation_id_p));
                active_repo.private = Set(repo.private);
                active_repo.updated_at = Set(chrono::Utc::now());
                active_repo.pushed_at = Set(repo.pushed_at.unwrap_or_else(chrono::Utc::now));
                active_repo.stargazers_count = Set(repo.stargazers_count);
                active_repo.watchers_count = Set(repo.watchers_count);
                active_repo.language = Set(Some(repo.language.clone()));
                active_repo.default_branch = Set(repo.default_branch.clone());
                active_repo.update(&txn).await?;
            } else {
                // Insert new repository
                debug!("Inserting new repository: {}", repo.full_name);
                repo_model.save(&txn).await?;
            }
        }

        info!(
            "Completed storing {} repositories for installation {}",
            repos_count, installation_id_p
        );

        // Update last_synced_at
        self.update_last_synced_at(installation_id_p).await?;

        // Commit transaction
        txn.commit().await?;

        // Fetch all repositories for launching jobs
        let all_repositories = repositories::Entity::find()
            .filter(repositories::Column::InstallationId.eq(Some(installation_id_p)))
            .all(self.db.as_ref())
            .await?;

        // Launch jobs
        let s = self.queue_service.clone();
        for repo in all_repositories {
            s.send(temps_core::jobs::Job::UpdateRepoFramework(
                temps_core::jobs::UpdateRepoFrameworkJob { repo_id: repo.id },
            ))
            .await
            .expect("failed to push job");
        }

        info!(
            "Repository synchronization completed successfully for installation {}",
            installation_id_p
        );
        Ok(())
    }

    pub async fn create_github_app_installation(
        &self,
        app_id: i32,
        installation_id_p: i32,
    ) -> Result<git_provider_connections::Model, GithubAppServiceError> {
        let (octocrab, github_app_data) = self.get_github_app_client_by_app_id(app_id).await?;

        let installation = octocrab
            .apps()
            .installation(InstallationId(installation_id_p as u64))
            .await
            .map_err(|e| {
                error!("Failed to get installation: {:?}", e);
                GithubAppServiceError::GithubApiError(e.to_string())
            })?;
        // Get access token for the installation
        let create_access_token = CreateInstallationAccessToken::default();
        let gh_access_tokens_url = Url::parse(installation.access_tokens_url.as_ref().unwrap())
            .map_err(|e| GithubAppServiceError::GithubApiError(e.to_string()))?;

        let access: InstallationToken = octocrab
            .post(gh_access_tokens_url.path(), Some(&create_access_token))
            .await
            .map_err(|e| GithubAppServiceError::GithubApiError(e.to_string()))?;

        // Parse token expiration
        let expires_at = chrono::DateTime::parse_from_rfc3339(&access.expires_at.unwrap())
            .map_err(|e| GithubAppServiceError::GithubApiError(e.to_string()))?;
        let expires_at = expires_at.with_timezone(&chrono::Utc);

        // Create connection in git_provider_connections
        let connection = self
            .git_provider_manager
            .create_connection(
                github_app_data.provider_id,
                0, // No user_id for app installations
                installation.account.login.clone(),
                installation.account.r#type.clone(),
                Some(access.token),
                None, // No refresh token for GitHub App installations
                Some(installation_id_p.to_string()),
                Some(serde_json::json!({
                    "account_id": installation.account.id.0,
                    "repository_selection": installation.repository_selection,
                    "access_tokens_url": installation.access_tokens_url,
                    "repositories_url": installation.repositories_url,
                    "html_url": installation.html_url,
                })),
                Some(expires_at),
            )
            .await
            .map_err(|e| {
                GithubAppServiceError::Other(format!("Failed to create connection: {}", e))
            })?;

        // Pass the connection ID to sync_repositories
        self.sync_repositories(installation.id.0 as i32, Some(connection.id))
            .await?;
        info!("Created new GitHub App installation: {}", installation_id_p);
        Ok(connection)
    }

    pub async fn get_repositories_for_installation(
        &self,
        installation_id_p: i32,
        git_provider_connection_id: Option<i32>,
    ) -> Result<(GitHubAppData, Vec<Repository>), GithubAppServiceError> {
        info!(
            "Fetching repositories for installation_id: {}, connection_id: {:?}",
            installation_id_p, git_provider_connection_id
        );

        // Use the provided connection_id or try to find one for this installation
        let connection_id = if let Some(id) = git_provider_connection_id {
            info!("Using provided connection_id: {}", id);
            id
        } else {
            // Try to get connection by installation_id
            info!(
                "Looking up connection by installation_id: {}",
                installation_id_p
            );
            self.git_provider_manager
                .get_connection_by_installation_id(&installation_id_p.to_string())
                .await
                .map(|conn| {
                    info!(
                        "Found connection {} for installation {}",
                        conn.id, installation_id_p
                    );
                    conn.id
                })
                .map_err(|e| {
                    error!(
                        "Failed to find connection for installation {}: {}",
                        installation_id_p, e
                    );
                    GithubAppServiceError::NotFound(format!(
                        "No connection found for installation {}: {}",
                        installation_id_p, e
                    ))
                })?
        };

        let (octocrab, github_app, _) = self.get_installation_token_client(connection_id).await?;

        let mut all_repositories = Vec::new();
        let mut page = 1;
        let per_page = 100;

        loop {
            let params = vec![
                ("per_page", per_page.to_string()),
                ("page", page.to_string()),
            ];

            info!(
                "Fetching repositories page {} for installation {}",
                page, installation_id_p
            );
            let installed_repos: InstallationRepositories = octocrab
                .get("/installation/repositories", Some(&params))
                .await
                .map_err(|e| {
                    error!(
                        "Failed to fetch repositories page {} for installation {}: {}",
                        page, installation_id_p, e
                    );
                    GithubAppServiceError::GithubApiError(e.to_string())
                })?;
            let repos = installed_repos.repositories.clone();
            let repos_len = repos.len();

            info!(
                "Retrieved {} repositories on page {} for installation {}",
                repos_len, page, installation_id_p
            );

            let repositories: Vec<Repository> = repos
                .into_iter()
                .map(|repo| Repository {
                    package_manager: None,
                    id: repo.id.0,
                    default_branch: repo.default_branch.unwrap_or_default(),
                    name: repo.name,
                    owner: match repo.owner {
                        Some(owner) => owner.login,
                        None => "".to_string(),
                    },
                    full_name: repo.full_name.unwrap_or_default(),
                    private: repo.private.unwrap_or(false),
                    description: repo.description,
                    framework: Some(Framework::Unknown.to_string()),
                    installation_id: installation_id_p,
                    pushed_at: repo.pushed_at,
                    stargazers_count: repo.stargazers_count.map_or(0, |count| count as i32),
                    watchers_count: repo.watchers_count.map_or(0, |count| count as i32),
                    language: repo
                        .language
                        .as_ref()
                        .map(|s| s.to_string())
                        .unwrap_or("".to_string()),
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    git_provider_connection_id: None,
                })
                .collect();

            all_repositories.extend(repositories);

            if repos_len < per_page {
                break;
            }

            page += 1;
        }

        info!(
            "Successfully retrieved {} total repositories for installation {}",
            all_repositories.len(),
            installation_id_p
        );

        Ok((github_app, all_repositories))
    }
    pub async fn get_installations(
        &self,
    ) -> Result<Vec<GitHubInstallationData>, GithubAppServiceError> {
        let connections = self
            .git_provider_manager
            .get_all_github_app_installations()
            .await
            .map_err(|e| {
                GithubAppServiceError::Other(format!("Failed to get installations: {}", e))
            })?;

        let mut installations = Vec::new();
        for connection in connections {
            if let Some(installation_id_str) = &connection.installation_id {
                if let Ok(installation_id) = installation_id_str.parse::<i32>() {
                    installations.push(GitHubInstallationData {
                        connection_id: connection.id,
                        provider_id: connection.provider_id,
                        id: connection.id, // For backward compatibility
                        installation_id,
                        github_app_id: connection.provider_id, // For backward compatibility
                        account_name: connection.account_name,
                        account_type: connection.account_type,
                        account_id: 0, // Default to 0 - not available in connections
                        access_token: connection.access_token,
                        token_expires_at: connection.token_expires_at,
                        last_synced_at: connection.last_synced_at,
                        html_url: None, // Not available in connections
                        created_at: connection.created_at,
                        updated_at: connection.updated_at,
                        suspended_at: None, // Not available in connections
                        suspended_by: None, // Not available in connections
                    });
                }
            }
        }
        Ok(installations)
    }

    pub async fn create_github_app(
        &self,
        app_data: serde_json::Value,
        _source_id_p: Option<String>,
    ) -> Result<GitHubAppData, GithubAppServiceError> {
        // Extract data from the app_data JSON
        let name_val = app_data["name"].as_str().unwrap_or("").to_string();
        let app_id_val = app_data["id"].as_i64().unwrap_or(0) as i32;
        let client_id_val = app_data["client_id"].as_str().unwrap_or("").to_string();
        let client_secret_val = app_data["client_secret"].as_str().unwrap_or("").to_string();
        let webhook_secret_val = app_data["webhook_secret"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let private_key_val = app_data["pem"].as_str().unwrap_or("").to_string();

        // Extract html_url or slug to construct the GitHub App URL
        let app_url = if let Some(html_url) = app_data["html_url"].as_str() {
            html_url.to_string()
        } else if let Some(slug) = app_data["slug"].as_str() {
            format!("https://github.com/apps/{}", slug)
        } else {
            // Fallback: construct URL from name (convert to slug format)
            let slug = name_val.to_lowercase().replace(" ", "-");
            format!("https://github.com/apps/{}", slug)
        };

        // Check if this app already exists
        if let Ok(existing) = self
            .git_provider_manager
            .get_github_app_provider_by_app_id(app_id_val)
            .await
        {
            return Err(GithubAppServiceError::Conflict(format!(
                "GitHub App with id {} already exists as provider {}",
                app_id_val, existing.id
            )));
        }

        // Create auth method for GitHub App
        let auth_method = crate::services::git_provider::AuthMethod::GitHubApp {
            app_id: app_id_val,
            client_id: client_id_val.clone(),
            client_secret: client_secret_val.clone(),
            private_key: private_key_val.clone(),
            webhook_secret: webhook_secret_val.clone(),
        };

        // Create git provider with the GitHub App URL
        let provider = self
            .git_provider_manager
            .create_provider(
                format!("GitHub App - {}", name_val),
                crate::services::git_provider::GitProviderType::GitHub,
                auth_method,
                Some(app_url.clone()),
                Some("https://api.github.com".to_string()), // API URL for GitHub
                Some(webhook_secret_val.clone()),
                false, // not default unless it's the first provider
            )
            .await
            .map_err(|e| {
                GithubAppServiceError::Other(format!("Failed to create git provider: {}", e))
            })?;

        info!(
            "Created new GitHub App provider: {} for app {}",
            provider.id, name_val
        );

        // Return the app data
        Ok(GitHubAppData {
            provider_id: provider.id,
            id: provider.id, // For backward compatibility, set to provider_id
            app_id: app_id_val,
            name: name_val.clone(),
            slug: name_val.clone(), // Use name as slug for now
            client_id: client_id_val,
            client_secret: client_secret_val,
            private_key: private_key_val,
            webhook_secret: webhook_secret_val,
            url: app_url, // Use the extracted or constructed URL
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        })
    }

    pub async fn get_installation_by_id(
        &self,
        installation_id_p: i32,
    ) -> Result<GitHubInstallationData, GithubAppServiceError> {
        let connection = self
            .git_provider_manager
            .get_connection_by_installation_id(&installation_id_p.to_string())
            .await
            .map_err(|e| {
                GithubAppServiceError::NotFound(format!(
                    "GitHub App Installation with id {} not found: {}",
                    installation_id_p, e
                ))
            })?;

        Ok(GitHubInstallationData {
            connection_id: connection.id,
            provider_id: connection.provider_id,
            id: connection.id, // For backward compatibility, set to connection_id
            installation_id: installation_id_p,
            github_app_id: connection.provider_id, // For backward compatibility, set to provider_id
            account_name: connection.account_name,
            account_type: connection.account_type,
            account_id: 0, // Default to 0 if not available
            access_token: connection.access_token,
            token_expires_at: connection.token_expires_at,
            last_synced_at: connection.last_synced_at,
            html_url: None,                 // Not available in connections
            created_at: chrono::Utc::now(), // Default to current time
            updated_at: chrono::Utc::now(), // Default to current time
            suspended_at: None,             // Default to None
            suspended_by: None,             // Default to None
        })
    }

    pub async fn update_ping_received_at(
        &self,
        app_id_p: i32,
        ping_time: UtcDateTime,
    ) -> Result<(), GithubAppServiceError> {
        // Find the provider for this GitHub App
        let provider = self
            .git_provider_manager
            .get_github_app_provider_by_app_id(app_id_p)
            .await
            .map_err(|e| {
                GithubAppServiceError::NotFound(format!(
                    "GitHub App with app_id {} not found: {}",
                    app_id_p, e
                ))
            })?;

        // Update auth_config with ping_received_at
        let mut auth_config = provider.auth_config.clone();
        auth_config["ping_received_at"] = serde_json::json!(ping_time.to_rfc3339());

        // Update the provider with new auth_config
        let mut active_provider: git_providers::ActiveModel = provider.into();
        active_provider.auth_config = Set(auth_config);
        active_provider.updated_at = Set(chrono::Utc::now());
        active_provider
            .update(self.db.as_ref())
            .await
            .map_err(GithubAppServiceError::DatabaseError)?;

        info!(
            "Updated ping_received_at to {} for GitHub App with id: {}",
            ping_time, app_id_p
        );
        Ok(())
    }
    pub async fn process_installation(
        &self,
        installation_id_p: i32,
        app_id: Option<i32>,
    ) -> Result<String, GithubAppServiceError> {
        info!(
            "Processing installation for GitHub App, installation_id: {}, app_id: {:?}",
            installation_id_p, app_id
        );

        // Try to find which GitHub App this installation belongs to
        let app: GitHubAppData = if let Some(app_id) = app_id {
            // If app_id is provided, use it directly
            info!("Using provided app_id: {}", app_id);
            let provider = self
                .git_provider_manager
                .get_github_app_provider_by_app_id(app_id)
                .await
                .map_err(|e| {
                    GithubAppServiceError::NotFound(format!(
                        "GitHub App with app_id {} not found: {}",
                        app_id, e
                    ))
                })?;
            self.extract_github_app_data(&provider).await?
        } else {
            // If no app_id provided, we need to try each GitHub App to find which one owns this installation
            info!(
                "No app_id provided, trying to find which GitHub App owns installation {}",
                installation_id_p
            );

            let providers = self
                .git_provider_manager
                .get_github_app_providers()
                .await
                .map_err(|e| {
                    GithubAppServiceError::Other(format!(
                        "Failed to get GitHub App providers: {}",
                        e
                    ))
                })?;

            if providers.is_empty() {
                return Err(GithubAppServiceError::NotFound(
                    "No GitHub Apps configured".to_string(),
                ));
            }

            let mut found_app = None;

            // Try each GitHub App to see which one can access this installation
            for provider in providers {
                if let Ok(app_data) = self.extract_github_app_data(&provider).await {
                    if let Ok((octocrab, _)) =
                        self.get_github_app_client_by_app_id(app_data.app_id).await
                    {
                        // Try to fetch the installation with this app's credentials
                        if let Ok(installation) = octocrab
                            .apps()
                            .installation(InstallationId(installation_id_p as u64))
                            .await
                        {
                            // Check if this installation belongs to this app
                            if let Some(installation_app_id) = installation.app_id {
                                if app_data.app_id == installation_app_id.0 as i32 {
                                    info!("Found installation {} belongs to GitHub App: {} (app_id: {})",
                                        installation_id_p, app_data.name, installation_app_id.0);
                                    found_app = Some(app_data);
                                    break;
                                }
                            }
                        }
                        // If we get an error, this app doesn't own the installation, try next
                    }
                }
            }

            found_app.ok_or_else(|| {
                GithubAppServiceError::NotFound(format!(
                    "No GitHub App found that owns installation {}",
                    installation_id_p
                ))
            })?
        };

        info!(
            "Found GitHub App in database: {} (app_id: {})",
            app.name, app.app_id
        );

        // create_github_app_installation already creates the connection and syncs repositories
        let connection = self
            .create_github_app_installation(app.app_id, installation_id_p)
            .await?;

        // Return success - connection and repositories already handled
        info!(
            "Successfully processed installation {} for GitHub App: {}",
            installation_id_p, app.name
        );
        let redirect_uri = format!(
            "/git-providers/{}?connection_id={}&event=installation_created",
            connection.provider_id, connection.id
        );
        Ok(redirect_uri)
    }

    pub async fn get_all_github_apps(&self) -> Result<Vec<GitHubAppData>, GithubAppServiceError> {
        let providers = self
            .git_provider_manager
            .get_github_app_providers()
            .await
            .map_err(|e| {
                GithubAppServiceError::Other(format!("Failed to get GitHub App providers: {}", e))
            })?;

        let mut apps = Vec::new();
        for provider in providers {
            if let Ok(app_data) = self.extract_github_app_data(&provider).await {
                apps.push(app_data);
            }
        }

        info!("Retrieved {} GitHub Apps", apps.len());
        Ok(apps)
    }

    pub async fn validate_webhook_signature(
        &self,
        signature: Option<&str>,
        body: &[u8],
    ) -> Result<(), GithubAppServiceError> {
        // Try to validate with each GitHub App until one succeeds
        // This is necessary because we don't know which app the webhook is from
        let github_apps = self.get_all_github_apps().await?;

        if github_apps.is_empty() {
            return Err(GithubAppServiceError::NotFound(
                "No GitHub Apps configured".to_string(),
            ));
        }

        let original_signature = signature.ok_or(GithubAppServiceError::InvalidWebhookSignature)?;

        if !original_signature.starts_with("sha256=") {
            return Err(GithubAppServiceError::InvalidWebhookSignature);
        }

        // Try each GitHub App's webhook secret
        for github_app in github_apps {
            // webhook_secret is already decrypted in GitHubAppData
            let webhook_secret = github_app.webhook_secret;

            let mut mac = match Hmac::<Sha256>::new_from_slice(webhook_secret.as_bytes()) {
                Ok(mac) => mac,
                Err(_) => continue, // Skip this app if HMAC creation fails
            };

            mac.update(body);
            let result = mac.finalize();
            let code_bytes = result.into_bytes();
            let expected_signature = format!("sha256={}", hex::encode(code_bytes));

            if original_signature == expected_signature {
                debug!("Valid signature for GitHub App: {}", github_app.name);
                return Ok(());
            }
        }

        // If no app's webhook secret matched, return error
        debug!("No GitHub App webhook secret matched the signature");
        Err(GithubAppServiceError::InvalidWebhookSignature)
    }

    pub async fn delete_installation(
        &self,
        installation_id_p: i32,
    ) -> Result<(), GithubAppServiceError> {
        info!(
            "Deleting GitHub installation with ID: {}",
            installation_id_p
        );

        // Convert installation_id to string for database query
        let installation_id_str = installation_id_p.to_string();

        // Find all connections associated with this installation
        let connections = git_provider_connections::Entity::find()
            .filter(
                git_provider_connections::Column::InstallationId.eq(installation_id_str.clone()),
            )
            .all(self.db.as_ref())
            .await
            .map_err(|e| GithubAppServiceError::DatabaseError(e))?;

        if connections.is_empty() {
            warn!(
                "No connections found for installation {}, proceeding with deletion",
                installation_id_p
            );
        } else {
            // Check if any projects depend on these connections
            let connection_ids: Vec<i32> = connections.iter().map(|c| c.id).collect();

            let dependent_projects = projects::Entity::find()
                .filter(projects::Column::GitProviderConnectionId.is_in(connection_ids.clone()))
                .all(self.db.as_ref())
                .await
                .map_err(|e| GithubAppServiceError::DatabaseError(e))?;

            if !dependent_projects.is_empty() {
                let project_names: Vec<String> =
                    dependent_projects.iter().map(|p| p.name.clone()).collect();
                let conflict_msg = format!(
                    "Cannot delete installation {} - {} project(s) depend on it: {}",
                    installation_id_p,
                    dependent_projects.len(),
                    project_names.join(", ")
                );
                warn!("{}", conflict_msg);
                return Err(GithubAppServiceError::Conflict(conflict_msg));
            }
        }

        // Delete the installation through git_provider_manager
        self.git_provider_manager
            .delete_installation(installation_id_p)
            .await
            .map_err(|e| {
                GithubAppServiceError::Other(format!(
                    "Failed to delete GitHub installation {}: {}",
                    installation_id_p, e
                ))
            })?;

        info!(
            "Successfully deleted GitHub installation {} and its repositories",
            installation_id_p
        );
        Ok(())
    }

    /// Verify if a GitHub App installation still exists and is accessible
    /// Returns true if the installation is active, false if it has been deleted or suspended
    pub async fn verify_installation(
        &self,
        installation_id_p: i32,
    ) -> Result<bool, GithubAppServiceError> {
        debug!(
            "Verifying GitHub installation with ID: {}",
            installation_id_p
        );

        // Get the connection to find which app owns this installation
        let connection = self
            .git_provider_manager
            .get_connection_by_installation_id(&installation_id_p.to_string())
            .await
            .map_err(|e| {
                GithubAppServiceError::NotFound(format!(
                    "GitHub App Installation with id {} not found in database: {}",
                    installation_id_p, e
                ))
            })?;

        // Get the provider to extract app credentials
        let provider = self
            .git_provider_manager
            .get_provider(connection.provider_id)
            .await
            .map_err(|e| {
                GithubAppServiceError::NotFound(format!(
                    "GitHub App provider {} not found: {}",
                    connection.provider_id, e
                ))
            })?;

        let app_data = self.extract_github_app_data(&provider).await?;

        // Try to get the installation from GitHub API
        let (octocrab, _) = self
            .get_github_app_client_by_app_id(app_data.app_id)
            .await?;

        match octocrab
            .apps()
            .installation(InstallationId(installation_id_p as u64))
            .await
        {
            Ok(_installation) => {
                // Installation exists and is accessible
                debug!(
                    "Installation {} is active and accessible",
                    installation_id_p
                );
                Ok(true)
            }
            Err(e) => {
                // Check if it's a 404 (installation deleted) or other error
                let error_str = e.to_string();
                if error_str.contains("404") || error_str.contains("Not Found") {
                    info!(
                        "Installation {} no longer exists (404 from GitHub API)",
                        installation_id_p
                    );
                    Ok(false)
                } else {
                    // Other errors (rate limiting, network issues, etc.)
                    warn!(
                        "Error checking installation {}: {}",
                        installation_id_p, error_str
                    );
                    Err(GithubAppServiceError::GithubApiError(format!(
                        "Failed to verify installation: {}",
                        error_str
                    )))
                }
            }
        }
    }

    /// Check and deactivate installations that are no longer accessible
    /// This should be called periodically (e.g., via a cron job)
    /// Returns the number of installations that were deactivated
    pub async fn check_and_deactivate_stale_installations(
        &self,
    ) -> Result<usize, GithubAppServiceError> {
        info!("Checking for stale GitHub installations");

        // Get all GitHub providers and their connections
        let providers = self
            .git_provider_manager
            .get_github_app_providers()
            .await
            .map_err(|e| {
                GithubAppServiceError::Other(format!("Failed to get GitHub providers: {}", e))
            })?;

        let mut all_connections = Vec::new();
        for provider in providers {
            let connections = self
                .git_provider_manager
                .get_provider_connections(provider.id)
                .await
                .map_err(|e| {
                    GithubAppServiceError::Other(format!(
                        "Failed to get connections for provider {}: {}",
                        provider.id, e
                    ))
                })?;
            all_connections.extend(connections);
        }

        let mut deactivated_count = 0;

        for connection in all_connections {
            // Skip non-GitHub or already inactive connections
            if !connection.is_active {
                continue;
            }

            // Parse installation_id from installation_id field
            let installation_id = match &connection.installation_id {
                Some(id_str) => match id_str.parse::<i32>() {
                    Ok(id) => id,
                    Err(_) => {
                        warn!(
                            "Invalid installation_id format for connection {}: {}",
                            connection.id, id_str
                        );
                        continue;
                    }
                },
                None => {
                    // Skip connections without installation_id (OAuth connections)
                    continue;
                }
            };

            // Verify if the installation still exists
            match self.verify_installation(installation_id).await {
                Ok(is_active) => {
                    if !is_active {
                        info!(
                            "Installation {} is no longer active, marking as disabled",
                            installation_id
                        );

                        // Deactivate the installation
                        if let Err(e) = self.delete_installation(installation_id).await {
                            error!(
                                "Failed to deactivate installation {}: {}",
                                installation_id, e
                            );
                        } else {
                            deactivated_count += 1;
                        }
                    }
                }
                Err(e) => {
                    // Log the error but continue checking other installations
                    warn!("Failed to verify installation {}: {}", installation_id, e);
                }
            }
        }

        info!(
            "Finished checking installations, deactivated {} installations",
            deactivated_count
        );
        Ok(deactivated_count)
    }

    /// Create a GitHubSource for framework detection and file access
    ///
    /// # Arguments
    /// * `provider_id` - GitHub App provider ID
    /// * `owner` - Repository owner (username or organization)
    /// * `repo` - Repository name
    /// * `reference` - Branch name, tag, or commit SHA
    /// Find which GitHub App provider owns a given installation
    /// Returns Ok(Some(provider_id)) if found, Ok(None) if not found
    pub async fn find_provider_for_installation(
        &self,
        installation_id_p: i32,
    ) -> Result<Option<i32>, GithubAppServiceError> {
        info!(
            "Finding GitHub App provider for installation {}",
            installation_id_p
        );

        let providers = self
            .git_provider_manager
            .get_github_app_providers()
            .await
            .map_err(|e| {
                GithubAppServiceError::Other(format!("Failed to get GitHub App providers: {}", e))
            })?;

        if providers.is_empty() {
            debug!("No GitHub App providers configured");
            return Ok(None);
        }

        // Try each GitHub App to see which one owns this installation
        for provider in providers {
            match self.extract_github_app_data(&provider).await {
                Ok(app_data) => {
                    debug!(
                        "Trying GitHub App {} (app_id: {}) for installation {}",
                        app_data.name, app_data.app_id, installation_id_p
                    );

                    // Get client for this app
                    match self.get_github_app_client_by_app_id(app_data.app_id).await {
                        Ok((octocrab, _)) => {
                            // Try to fetch the installation with this app's credentials
                            match octocrab
                                .apps()
                                .installation(InstallationId(installation_id_p as u64))
                                .await
                            {
                                Ok(installation) => {
                                    // Check if this installation belongs to this app
                                    if let Some(installation_app_id) = installation.app_id {
                                        if app_data.app_id == installation_app_id.0 as i32 {
                                            info!(
                                                "Found: Installation {} belongs to GitHub App {} (provider_id: {})",
                                                installation_id_p, app_data.name, provider.id
                                            );
                                            return Ok(Some(provider.id));
                                        } else {
                                            debug!(
                                                "Installation {} app_id {} doesn't match provider app_id {}",
                                                installation_id_p, installation_app_id.0, app_data.app_id
                                            );
                                        }
                                    }
                                }
                                Err(e) => {
                                    debug!(
                                        "Installation {} not accessible with app {}: {}",
                                        installation_id_p, app_data.name, e
                                    );
                                    // This app doesn't own the installation, try next
                                }
                            }
                        }
                        Err(e) => {
                            debug!(
                                "Failed to get client for app {} (app_id: {}): {}",
                                app_data.name, app_data.app_id, e
                            );
                            // Skip this provider, try next
                        }
                    }
                }
                Err(e) => {
                    debug!(
                        "Failed to extract app data for provider {}: {}",
                        provider.id, e
                    );
                    // Skip this provider, try next
                }
            }
        }

        info!(
            "No GitHub App provider found for installation {}",
            installation_id_p
        );
        Ok(None)
    }

    pub async fn create_source(
        &self,
        provider_id: i32,
        owner: String,
        repo: String,
        reference: String,
    ) -> Result<crate::sources::GitHubSource, GithubAppServiceError> {
        // Get provider and extract app data
        let provider = self
            .git_provider_manager
            .get_provider(provider_id)
            .await
            .map_err(|e| GithubAppServiceError::Other(format!("Failed to get provider: {}", e)))?;

        let app_data = self.extract_github_app_data(&provider).await?;

        // Create Octocrab client using installation token
        // We need a connection ID, which we don't have here
        // Instead, create Octocrab client using the app's private key
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(app_data.private_key.as_bytes())
            .map_err(|e| {
                GithubAppServiceError::PrivateKeyCreationFailed(format!(
                    "Failed to create encoding key: {}",
                    e
                ))
            })?;

        let octocrab = Octocrab::builder()
            .app(AppId::from(app_data.app_id as u64), key)
            .build()
            .map_err(|e| {
                GithubAppServiceError::GithubApiError(format!(
                    "Failed to create Octocrab client: {}",
                    e
                ))
            })?;

        Ok(crate::sources::GitHubSource::new(
            Arc::new(octocrab),
            owner,
            repo,
            reference,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GitTreeRecursive {
    /// The SHA hash of the tree
    pub sha: String,
    /// The API URL for this tree
    pub url: Url,
    /// The collection of tree entries
    pub tree: Vec<GitTreeEntryRecursive>,
    /// Whether the response was truncated due to size limitations
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GitTreeEntryRecursive {
    /// The file path relative to the root of the tree
    pub path: String,
    /// The mode of the file/directory (100644 for file, 040000 for directory, etc.)
    pub mode: String,
    /// The type of tree entry (blob, tree, commit)
    #[serde(rename = "type")]
    pub type_: GitTreeEntryType,
    /// The size of the file in bytes (only present for blobs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// The SHA hash of the entry
    pub sha: String,
    /// The API URL for this entry
    pub url: Url,
    /// Child entries if this is a tree (directory)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<GitTreeEntryRecursive>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum GitTreeEntryType {
    Blob,
    Tree,
    Commit,
}

#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub sha: String,
    pub message: String,
    pub committer_name: String,
    pub committer_email: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: String,
    refresh_token_expires_in: i64,
    scope: String,
    token_type: String,
}
#[derive(Debug, Clone)]
pub struct GithubAccountDetails {
    pub account_name: String,
    pub account_id: i64,
    pub is_org: bool,
}
