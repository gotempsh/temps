use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::UtcDateTime;
use thiserror::Error;
use utoipa::ToSchema;

#[derive(Error, Debug)]
pub enum GitProviderError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),

    #[error("Provider not found: {0}")]
    ProviderNotFound(String),

    #[error("Connection not found: {0}")]
    ConnectionNotFound(String),

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("API error: {0}")]
    ApiError(String),

    #[error("Not implemented for this provider")]
    NotImplemented,

    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),

    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    #[error("Other error: {0}")]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GitProviderType {
    GitHub,
    GitLab,
    Bitbucket,
    Gitea,
    Generic,
}

impl std::fmt::Display for GitProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitProviderType::GitHub => write!(f, "github"),
            GitProviderType::GitLab => write!(f, "gitlab"),
            GitProviderType::Bitbucket => write!(f, "bitbucket"),
            GitProviderType::Gitea => write!(f, "gitea"),
            GitProviderType::Generic => write!(f, "generic"),
        }
    }
}

impl TryFrom<&str> for GitProviderType {
    type Error = GitProviderError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value.to_lowercase().as_str() {
            "github" => Ok(GitProviderType::GitHub),
            "gitlab" => Ok(GitProviderType::GitLab),
            "bitbucket" => Ok(GitProviderType::Bitbucket),
            "gitea" => Ok(GitProviderType::Gitea),
            "generic" => Ok(GitProviderType::Generic),
            _ => Err(GitProviderError::InvalidConfiguration(format!(
                "Unknown provider type: {}",
                value
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum AuthMethod {
    GitHubApp {
        app_id: i32,
        client_id: String,
        client_secret: String,
        private_key: String,
        webhook_secret: String,
    },
    GitLabApp {
        app_id: String,
        app_secret: String,
        redirect_uri: String,
    },
    OAuth {
        client_id: String,
        client_secret: String,
        redirect_uri: String,
    },
    PersonalAccessToken {
        token: String,
    },
    BasicAuth {
        username: String,
        password: String,
    },
    SSHKey {
        private_key: String,
        public_key: String,
    },
}

// Custom deserializer to handle both tagged and untagged formats
impl<'de> Deserialize<'de> for AuthMethod {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};
        use serde_json::Value;

        struct AuthMethodVisitor;

        impl<'de> Visitor<'de> for AuthMethodVisitor {
            type Value = AuthMethod;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("AuthMethod in either tagged or untagged format")
            }

            fn visit_map<V>(self, mut map: V) -> Result<AuthMethod, V::Error>
            where
                V: MapAccess<'de>,
            {
                // First, collect all entries into a serde_json::Value
                let mut json_map = serde_json::Map::new();
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    json_map.insert(key, value);
                }
                let json_value = Value::Object(json_map);

                // Check if it's in tagged format (has a single key that's the variant name)
                if let Value::Object(ref obj) = json_value {
                    // Check for tagged format - single key that matches a variant name
                    if obj.len() == 1 || (obj.len() == 2 && obj.contains_key("ping_received_at")) {
                        if let Some(inner) = obj.get("GitHubApp") {
                            if let Ok(github_app) =
                                serde_json::from_value::<GitHubAppFields>(inner.clone())
                            {
                                return Ok(AuthMethod::GitHubApp {
                                    app_id: github_app.app_id,
                                    client_id: github_app.client_id,
                                    client_secret: github_app.client_secret,
                                    private_key: github_app.private_key,
                                    webhook_secret: github_app.webhook_secret,
                                });
                            }
                        }
                        if let Some(inner) = obj.get("GitLabApp") {
                            if let Ok(gitlab_app) =
                                serde_json::from_value::<GitLabAppFields>(inner.clone())
                            {
                                return Ok(AuthMethod::GitLabApp {
                                    app_id: gitlab_app.app_id,
                                    app_secret: gitlab_app.app_secret,
                                    redirect_uri: gitlab_app.redirect_uri,
                                });
                            }
                        }
                        if let Some(inner) = obj.get("OAuth") {
                            if let Ok(oauth) = serde_json::from_value::<OAuthFields>(inner.clone())
                            {
                                return Ok(AuthMethod::OAuth {
                                    client_id: oauth.client_id,
                                    client_secret: oauth.client_secret,
                                    redirect_uri: oauth.redirect_uri,
                                });
                            }
                        }
                        if let Some(inner) = obj.get("PersonalAccessToken") {
                            if let Ok(pat) =
                                serde_json::from_value::<PersonalAccessTokenFields>(inner.clone())
                            {
                                return Ok(AuthMethod::PersonalAccessToken { token: pat.token });
                            }
                        }
                        if let Some(inner) = obj.get("BasicAuth") {
                            if let Ok(basic) =
                                serde_json::from_value::<BasicAuthFields>(inner.clone())
                            {
                                return Ok(AuthMethod::BasicAuth {
                                    username: basic.username,
                                    password: basic.password,
                                });
                            }
                        }
                        if let Some(inner) = obj.get("SSHKey") {
                            if let Ok(ssh) = serde_json::from_value::<SSHKeyFields>(inner.clone()) {
                                return Ok(AuthMethod::SSHKey {
                                    private_key: ssh.private_key,
                                    public_key: ssh.public_key,
                                });
                            }
                        }
                    }

                    // Try untagged format - fields directly in the object
                    // Try each variant in order
                    if let Ok(github_app) =
                        serde_json::from_value::<GitHubAppFields>(json_value.clone())
                    {
                        return Ok(AuthMethod::GitHubApp {
                            app_id: github_app.app_id,
                            client_id: github_app.client_id,
                            client_secret: github_app.client_secret,
                            private_key: github_app.private_key,
                            webhook_secret: github_app.webhook_secret,
                        });
                    }
                    if let Ok(gitlab_app) =
                        serde_json::from_value::<GitLabAppFields>(json_value.clone())
                    {
                        return Ok(AuthMethod::GitLabApp {
                            app_id: gitlab_app.app_id,
                            app_secret: gitlab_app.app_secret,
                            redirect_uri: gitlab_app.redirect_uri,
                        });
                    }
                    if let Ok(oauth) = serde_json::from_value::<OAuthFields>(json_value.clone()) {
                        return Ok(AuthMethod::OAuth {
                            client_id: oauth.client_id,
                            client_secret: oauth.client_secret,
                            redirect_uri: oauth.redirect_uri,
                        });
                    }
                    if let Ok(pat) =
                        serde_json::from_value::<PersonalAccessTokenFields>(json_value.clone())
                    {
                        return Ok(AuthMethod::PersonalAccessToken { token: pat.token });
                    }
                    if let Ok(basic) = serde_json::from_value::<BasicAuthFields>(json_value.clone())
                    {
                        return Ok(AuthMethod::BasicAuth {
                            username: basic.username,
                            password: basic.password,
                        });
                    }
                    if let Ok(ssh) = serde_json::from_value::<SSHKeyFields>(json_value.clone()) {
                        return Ok(AuthMethod::SSHKey {
                            private_key: ssh.private_key,
                            public_key: ssh.public_key,
                        });
                    }
                }

                Err(de::Error::custom(
                    "data did not match any variant of AuthMethod",
                ))
            }
        }

        deserializer.deserialize_map(AuthMethodVisitor)
    }
}

// Helper structs for deserialization
#[derive(Deserialize)]
struct GitHubAppFields {
    app_id: i32,
    client_id: String,
    client_secret: String,
    private_key: String,
    webhook_secret: String,
}

#[derive(Deserialize)]
struct GitLabAppFields {
    app_id: String,
    app_secret: String,
    redirect_uri: String,
}

#[derive(Deserialize)]
struct OAuthFields {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

#[derive(Deserialize)]
struct PersonalAccessTokenFields {
    token: String,
}

#[derive(Deserialize)]
struct BasicAuthFields {
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct SSHKeyFields {
    private_key: String,
    public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub id: String, // Provider-specific ID
    pub name: String,
    pub full_name: String,
    pub owner: String,
    pub description: Option<String>,
    pub private: bool,
    pub default_branch: String,
    pub clone_url: String,
    pub ssh_url: String,
    pub web_url: String,
    pub language: Option<String>,
    pub size: i64,
    pub stars: i32,
    pub forks: i32,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub pushed_at: Option<UtcDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub name: String,
    pub commit_sha: String,
    pub protected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    pub message: String,
    pub author: String,
    pub author_email: String,
    pub date: UtcDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub encoding: String, // base64, utf-8, etc.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub url: String,
    pub secret: Option<String>,
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
}

/// Trait that all git providers must implement
#[async_trait]
pub trait GitProviderService: Send + Sync {
    /// Get the provider type
    fn provider_type(&self) -> GitProviderType;

    /// Authenticate and get access token
    async fn authenticate(&self, code: Option<String>) -> Result<String, GitProviderError>;

    /// Get authorization URL for OAuth flow
    async fn get_auth_url(&self, state: &str) -> Result<String, GitProviderError>;

    /// Check if the access token needs to be refreshed (expired or invalid)
    /// Returns true if the token is expired or invalid and should be refreshed
    async fn token_needs_refresh(&self, access_token: &str) -> bool;

    /// Validate an access token by checking with the provider API
    /// Returns true if the token is valid, false otherwise
    async fn validate_token(&self, access_token: &str) -> Result<bool, GitProviderError>;

    /// Validate the access token and refresh if needed
    /// Returns (access_token, Option<refresh_token>) - refresh_token is Some if it was refreshed
    async fn validate_and_refresh_token(
        &self,
        access_token: &str,
        refresh_token: Option<&str>,
    ) -> Result<(String, Option<String>), GitProviderError>;

    /// List repositories for authenticated user/organization
    async fn list_repositories(
        &self,
        access_token: &str,
        organization: Option<&str>,
    ) -> Result<Vec<Repository>, GitProviderError>;

    /// Get a specific repository
    async fn get_repository(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Repository, GitProviderError>;

    /// List branches for a repository
    async fn list_branches(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<Branch>, GitProviderError>;

    /// List tags for a repository
    async fn list_tags(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<GitProviderTag>, GitProviderError>;

    /// Get file content from repository
    async fn get_file_content(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        path: &str,
        branch: Option<&str>,
    ) -> Result<FileContent, GitProviderError>;

    /// Get latest commit for a branch
    async fn get_latest_commit(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<Commit, GitProviderError>;

    /// Get a specific commit by SHA or reference (branch/tag)
    async fn get_commit(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        reference: &str, // Can be commit SHA, branch name, or tag name
    ) -> Result<Commit, GitProviderError>;

    /// Check if a commit exists in the repository
    async fn check_commit_exists(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        commit_sha: &str,
    ) -> Result<bool, GitProviderError>;

    /// Create a webhook for repository
    async fn create_webhook(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        config: WebhookConfig,
    ) -> Result<String, GitProviderError>;

    /// Delete a webhook
    async fn delete_webhook(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        webhook_id: &str,
    ) -> Result<(), GitProviderError>;

    /// Verify webhook signature
    async fn verify_webhook_signature(
        &self,
        payload: &[u8],
        signature: &str,
        secret: &str,
    ) -> Result<bool, GitProviderError>;

    /// Get authenticated user information
    async fn get_user(&self, access_token: &str) -> Result<User, GitProviderError>;

    /// Check if a repository is accessible (for public repos without auth)
    async fn check_repository_accessible(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<bool, GitProviderError>;

    /// Clone repository using git command (for non-API access)
    async fn clone_repository(
        &self,
        clone_url: &str,
        target_dir: &str,
        access_token: Option<&str>,
    ) -> Result<(), GitProviderError>;

    /// Download repository archive (tarball/zip) for a specific ref (branch, tag, or commit)
    async fn download_archive(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        ref_spec: &str, // Can be branch name, tag, or commit SHA
        target_path: &std::path::Path,
    ) -> Result<(), GitProviderError>;

    /// Create a ProjectSource for framework detection and file access
    ///
    /// This method creates a provider-specific ProjectSource that allows
    /// framework detection and configuration without cloning the repository.
    /// The source makes on-demand API calls to fetch files as needed.
    ///
    /// # Arguments
    /// * `access_token` - Access token for authentication
    /// * `owner` - Repository owner (username or organization)
    /// * `repo` - Repository name
    /// * `reference` - Branch name, tag, or commit SHA
    ///
    /// # Example
    /// ```ignore
    /// use temps_git::services::git_provider::{GitProviderService, GitProviderFactory};
    /// use temps_presets::frameworks::async_provider::AsyncProviderRegistry;
    ///
    /// async fn detect_framework(provider: &dyn GitProviderService, access_token: &str) {
    ///     // Create a ProjectSource from the provider
    ///     let source = provider
    ///         .create_source(access_token, "owner", "repo", "main")
    ///         .await
    ///         .unwrap();
    ///
    ///     // Use with framework detection
    ///     let detector = AsyncProviderRegistry::new();
    ///     let detection = detector.detect(source.as_ref()).await;
    ///
    ///     println!("Detected framework: {:?}", detection);
    /// }
    /// ```
    async fn create_source(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
        reference: &str,
    ) -> Result<Box<dyn temps_presets::source::ProjectSource>, GitProviderError>;
}

/// Factory for creating provider instances
pub struct GitProviderFactory;

impl GitProviderFactory {
    pub async fn create_provider(
        provider_type: GitProviderType,
        auth_method: AuthMethod,
        base_url: Option<String>,
        api_url: Option<String>,
        _db: Arc<DatabaseConnection>,
    ) -> Result<Box<dyn GitProviderService>, GitProviderError> {
        match provider_type {
            GitProviderType::GitHub => {
                use crate::services::github_provider::GitHubProvider;
                Ok(Box::new(GitHubProvider::new(api_url, auth_method)))
            }
            GitProviderType::GitLab => {
                use crate::services::gitlab_provider::GitLabProvider;
                Ok(Box::new(GitLabProvider::new(base_url, auth_method)))
            }
            GitProviderType::Bitbucket => {
                // Future implementation
                Err(GitProviderError::NotImplemented)
            }
            GitProviderType::Gitea => {
                // Future implementation
                Err(GitProviderError::NotImplemented)
            }
            GitProviderType::Generic => {
                // Future implementation for generic git servers
                Err(GitProviderError::NotImplemented)
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GitProviderRepository {
    pub id: String,
    pub name: String,
    pub full_name: String,
    pub owner: String,
    pub description: Option<String>,
    pub private: bool,
    pub default_branch: String,
    pub clone_url: String,
    pub ssh_url: String,
    pub web_url: String,
    pub language: Option<String>,
    pub size: i64,
    pub stars: i32,
    pub forks: i32,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: UtcDateTime,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: UtcDateTime,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub pushed_at: Option<UtcDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GitProviderBranch {
    pub name: String,
    pub commit_sha: String,
    pub protected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GitProviderTag {
    pub name: String,
    pub commit_sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RepositoryListParams {
    pub page: Option<u64>,
    pub per_page: Option<u64>,
    pub sort: Option<String>,
    pub direction: Option<String>,
    pub organization: Option<String>,
    pub search_term: Option<String>,
}

impl From<Repository> for GitProviderRepository {
    fn from(repo: Repository) -> Self {
        Self {
            id: repo.id,
            name: repo.name,
            full_name: repo.full_name,
            owner: repo.owner,
            description: repo.description,
            private: repo.private,
            default_branch: repo.default_branch,
            clone_url: repo.clone_url,
            ssh_url: repo.ssh_url,
            web_url: repo.web_url,
            language: repo.language,
            size: repo.size,
            stars: repo.stars,
            forks: repo.forks,
            created_at: repo.created_at,
            updated_at: repo.updated_at,
            pushed_at: repo.pushed_at,
        }
    }
}

impl From<Branch> for GitProviderBranch {
    fn from(branch: Branch) -> Self {
        Self {
            name: branch.name,
            commit_sha: branch.commit_sha,
            protected: branch.protected,
        }
    }
}

#[async_trait]
pub trait GitProviderRepositoryService: Send + Sync {
    async fn list_repositories(
        &self,
        access_token: &str,
        params: RepositoryListParams,
    ) -> Result<Vec<GitProviderRepository>, GitProviderError>;

    async fn get_repository(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<GitProviderRepository, GitProviderError>;

    async fn list_branches(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<GitProviderBranch>, GitProviderError>;

    async fn list_tags(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<GitProviderTag>, GitProviderError>;
}
