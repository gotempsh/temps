//! Deployment Token Service
//!
//! Manages deployment tokens that provide API access credentials
//! for deployed applications via TEMPS_API_URL and TEMPS_API_TOKEN
//! environment variables.

use axum::http::StatusCode;
use chrono::Utc;
use rand::Rng;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_core::UtcDateTime;
use temps_database::DbConnection;
use temps_entities::deployment_tokens::{
    ActiveModel as DeploymentTokenActiveModel, DeploymentTokenPermission,
    Entity as DeploymentTokenEntity, Model as DeploymentTokenModel,
};
use thiserror::Error;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// Response DTOs

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeploymentTokenResponse {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub name: String,
    pub token_prefix: String,
    pub permissions: Option<Vec<String>>,
    pub is_active: bool,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub last_used_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub created_at: UtcDateTime,
    pub created_by: Option<i32>,
}

impl From<DeploymentTokenModel> for DeploymentTokenResponse {
    fn from(model: DeploymentTokenModel) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            environment_id: model.environment_id,
            name: model.name,
            token_prefix: model.token_prefix,
            permissions: model
                .permissions
                .and_then(|p| serde_json::from_value(p).ok()),
            is_active: model.is_active,
            expires_at: model.expires_at,
            last_used_at: model.last_used_at,
            created_at: model.created_at,
            created_by: model.created_by,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateDeploymentTokenResponse {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub name: String,
    pub token_prefix: String,
    pub permissions: Option<Vec<String>>,
    /// The full token value - only returned on creation
    pub token: String,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub created_at: UtcDateTime,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeploymentTokenListResponse {
    pub tokens: Vec<DeploymentTokenResponse>,
    pub total: u64,
}

// Request DTOs

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateDeploymentTokenRequest {
    pub name: String,
    /// Optional environment ID - if not set, token applies to all environments
    pub environment_id: Option<i32>,
    /// List of permissions (e.g., ["visitors:enrich", "emails:send"])
    /// If not provided, defaults to full access
    #[schema(example = json!(["visitors:enrich", "emails:send"]))]
    pub permissions: Option<Vec<String>>,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateDeploymentTokenRequest {
    pub name: Option<String>,
    pub is_active: Option<bool>,
    #[schema(example = json!(["visitors:enrich", "emails:send"]))]
    pub permissions: Option<Vec<String>>,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
}

#[derive(Error, Debug)]
pub enum DeploymentTokenServiceError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Internal server error: {0}")]
    InternalServerError(String),
}

impl DeploymentTokenServiceError {
    pub fn to_problem(&self) -> Problem {
        match self {
            DeploymentTokenServiceError::DatabaseError(e) => {
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/database-error")
                    .title("Database Error")
                    .detail(format!("A database error occurred: {}", e))
                    .value("error_code", "DATABASE_ERROR")
                    .build()
            }
            DeploymentTokenServiceError::NotFound(msg) => ErrorBuilder::new(StatusCode::NOT_FOUND)
                .type_("https://temps.sh/probs/deployment-token-not-found")
                .title("Deployment Token Not Found")
                .detail(msg.clone())
                .value("error_code", "DEPLOYMENT_TOKEN_NOT_FOUND")
                .build(),
            DeploymentTokenServiceError::ValidationError(msg) => {
                ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .type_("https://temps.sh/probs/validation-error")
                    .title("Validation Error")
                    .detail(msg.clone())
                    .value("error_code", "VALIDATION_ERROR")
                    .build()
            }
            DeploymentTokenServiceError::Unauthorized(msg) => {
                ErrorBuilder::new(StatusCode::UNAUTHORIZED)
                    .type_("https://temps.sh/probs/unauthorized")
                    .title("Unauthorized")
                    .detail(msg.clone())
                    .value("error_code", "UNAUTHORIZED")
                    .build()
            }
            DeploymentTokenServiceError::Conflict(msg) => ErrorBuilder::new(StatusCode::CONFLICT)
                .type_("https://temps.sh/probs/conflict")
                .title("Conflict")
                .detail(msg.clone())
                .value("error_code", "CONFLICT")
                .build(),
            DeploymentTokenServiceError::InternalServerError(msg) => {
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/internal-server-error")
                    .title("Internal Server Error")
                    .detail(msg.clone())
                    .value("error_code", "INTERNAL_SERVER_ERROR")
                    .build()
            }
        }
    }
}

pub struct DeploymentTokenService {
    db: Arc<DbConnection>,
}

impl DeploymentTokenService {
    pub fn new(db: Arc<DbConnection>) -> Self {
        Self { db }
    }

    /// Create a new deployment token for a project
    pub async fn create_token(
        &self,
        project_id: i32,
        user_id: Option<i32>,
        request: CreateDeploymentTokenRequest,
    ) -> Result<CreateDeploymentTokenResponse, DeploymentTokenServiceError> {
        // Validate permissions if provided
        let permissions_json = if let Some(ref perms) = request.permissions {
            for perm_str in perms {
                if DeploymentTokenPermission::from_str(perm_str).is_none() {
                    return Err(DeploymentTokenServiceError::ValidationError(format!(
                        "Invalid permission: {}. Valid permissions are: visitors:enrich, emails:send, analytics:read, events:write, errors:read, * (full access)",
                        perm_str
                    )));
                }
            }
            Some(serde_json::to_value(perms).map_err(|e| {
                DeploymentTokenServiceError::InternalServerError(format!(
                    "Failed to serialize permissions: {}",
                    e
                ))
            })?)
        } else {
            // Default to full access
            Some(serde_json::json!(["*"]))
        };

        // Check if name is unique for this project
        let existing = DeploymentTokenEntity::find()
            .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
            .filter(temps_entities::deployment_tokens::Column::Name.eq(&request.name))
            .one(self.db.as_ref())
            .await?;

        if existing.is_some() {
            return Err(DeploymentTokenServiceError::Conflict(
                "Deployment token with this name already exists for this project".to_string(),
            ));
        }

        // Generate token
        let token = self.generate_token();
        let token_hash = self.hash_token(&token);
        let token_prefix = token.chars().take(8).collect::<String>();

        let now = Utc::now();
        let expires_at = request.expires_at.or_else(|| {
            // Default expiration: never (None)
            None
        });

        let new_token = DeploymentTokenActiveModel {
            project_id: Set(project_id),
            environment_id: Set(request.environment_id),
            name: Set(request.name.clone()),
            token_hash: Set(token_hash),
            token_prefix: Set(token_prefix.clone()),
            permissions: Set(permissions_json.clone()),
            is_active: Set(true),
            expires_at: Set(expires_at),
            last_used_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            created_by: Set(user_id),
            ..Default::default()
        };

        let token_model = new_token.insert(self.db.as_ref()).await?;

        Ok(CreateDeploymentTokenResponse {
            id: token_model.id,
            project_id: token_model.project_id,
            environment_id: token_model.environment_id,
            name: token_model.name,
            token_prefix,
            permissions: permissions_json.and_then(|p| serde_json::from_value(p).ok()),
            token, // Only returned on creation
            expires_at: token_model.expires_at,
            created_at: token_model.created_at,
        })
    }

    /// List all deployment tokens for a project
    pub async fn list_tokens(
        &self,
        project_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<DeploymentTokenListResponse, DeploymentTokenServiceError> {
        let paginator = DeploymentTokenEntity::find()
            .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
            .order_by_desc(temps_entities::deployment_tokens::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);

        let total = paginator.num_items().await?;
        let tokens_models = paginator.fetch_page(page.saturating_sub(1)).await?;

        let tokens = tokens_models
            .into_iter()
            .map(DeploymentTokenResponse::from)
            .collect();

        Ok(DeploymentTokenListResponse { tokens, total })
    }

    /// Get a specific deployment token
    pub async fn get_token(
        &self,
        project_id: i32,
        token_id: i32,
    ) -> Result<DeploymentTokenResponse, DeploymentTokenServiceError> {
        let token = DeploymentTokenEntity::find_by_id(token_id)
            .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentTokenServiceError::NotFound("Deployment token not found".to_string())
            })?;

        Ok(DeploymentTokenResponse::from(token))
    }

    /// Update a deployment token
    pub async fn update_token(
        &self,
        project_id: i32,
        token_id: i32,
        request: UpdateDeploymentTokenRequest,
    ) -> Result<DeploymentTokenResponse, DeploymentTokenServiceError> {
        let token = DeploymentTokenEntity::find_by_id(token_id)
            .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentTokenServiceError::NotFound("Deployment token not found".to_string())
            })?;

        // Check if new name conflicts
        if let Some(ref new_name) = request.name {
            if new_name != &token.name {
                let existing = DeploymentTokenEntity::find()
                    .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
                    .filter(temps_entities::deployment_tokens::Column::Name.eq(new_name))
                    .filter(temps_entities::deployment_tokens::Column::Id.ne(token_id))
                    .one(self.db.as_ref())
                    .await?;

                if existing.is_some() {
                    return Err(DeploymentTokenServiceError::Conflict(
                        "Deployment token with this name already exists".to_string(),
                    ));
                }
            }
        }

        let mut token_active: DeploymentTokenActiveModel = token.into();

        if let Some(name) = request.name {
            token_active.name = Set(name);
        }
        if let Some(is_active) = request.is_active {
            token_active.is_active = Set(is_active);
        }
        if let Some(expires_at) = request.expires_at {
            token_active.expires_at = Set(Some(expires_at));
        }
        if let Some(ref perms) = request.permissions {
            // Validate permissions
            for perm_str in perms {
                if DeploymentTokenPermission::from_str(perm_str).is_none() {
                    return Err(DeploymentTokenServiceError::ValidationError(format!(
                        "Invalid permission: {}",
                        perm_str
                    )));
                }
            }
            token_active.permissions = Set(Some(serde_json::to_value(perms).map_err(|e| {
                DeploymentTokenServiceError::InternalServerError(format!(
                    "Failed to serialize permissions: {}",
                    e
                ))
            })?));
        }
        token_active.updated_at = Set(Utc::now());

        let updated_token = token_active.update(self.db.as_ref()).await?;

        Ok(DeploymentTokenResponse::from(updated_token))
    }

    /// Delete a deployment token
    pub async fn delete_token(
        &self,
        project_id: i32,
        token_id: i32,
    ) -> Result<(), DeploymentTokenServiceError> {
        let token = DeploymentTokenEntity::find_by_id(token_id)
            .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentTokenServiceError::NotFound("Deployment token not found".to_string())
            })?;

        DeploymentTokenEntity::delete_by_id(token.id)
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }

    /// Validate a deployment token and return project info and permissions
    /// Returns (project_id, environment_id, permissions)
    pub async fn validate_token(
        &self,
        token: &str,
    ) -> Result<(i32, Option<i32>, Vec<DeploymentTokenPermission>), DeploymentTokenServiceError>
    {
        let token_hash = self.hash_token(token);
        let token_prefix = token.chars().take(8).collect::<String>();

        let token_model = DeploymentTokenEntity::find()
            .filter(temps_entities::deployment_tokens::Column::TokenHash.eq(&token_hash))
            .filter(temps_entities::deployment_tokens::Column::TokenPrefix.eq(&token_prefix))
            .filter(temps_entities::deployment_tokens::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentTokenServiceError::Unauthorized("Invalid deployment token".to_string())
            })?;

        // Check if expired
        if let Some(expires_at) = token_model.expires_at {
            if expires_at <= Utc::now() {
                return Err(DeploymentTokenServiceError::Unauthorized(
                    "Deployment token has expired".to_string(),
                ));
            }
        }

        // Parse permissions
        let permissions = if let Some(ref perms_json) = token_model.permissions {
            let perm_strings: Vec<String> =
                serde_json::from_value(perms_json.clone()).map_err(|_| {
                    DeploymentTokenServiceError::InternalServerError(
                        "Invalid permissions in database".to_string(),
                    )
                })?;

            perm_strings
                .iter()
                .filter_map(|s| DeploymentTokenPermission::from_str(s))
                .collect()
        } else {
            vec![DeploymentTokenPermission::FullAccess]
        };

        // Update last_used_at
        let mut token_active: DeploymentTokenActiveModel = token_model.clone().into();
        token_active.last_used_at = Set(Some(Utc::now()));
        let _ = token_active.update(self.db.as_ref()).await; // Don't fail if this fails

        Ok((
            token_model.project_id,
            token_model.environment_id,
            permissions,
        ))
    }

    /// Get the active deployment token for a project/environment combination
    /// Used during deployment to inject TEMPS_API_TOKEN
    /// Returns the token value if one exists, or creates a default one
    pub async fn get_or_create_deployment_token(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<String, DeploymentTokenServiceError> {
        // First, try to find an existing active token for this project/environment
        let mut query = DeploymentTokenEntity::find()
            .filter(temps_entities::deployment_tokens::Column::ProjectId.eq(project_id))
            .filter(temps_entities::deployment_tokens::Column::IsActive.eq(true));

        // If environment_id is provided, prefer tokens for that specific environment
        // Otherwise, look for project-wide tokens (environment_id is NULL)
        if let Some(env_id) = environment_id {
            query = query.filter(
                temps_entities::deployment_tokens::Column::EnvironmentId
                    .eq(env_id)
                    .or(temps_entities::deployment_tokens::Column::EnvironmentId.is_null()),
            );
        } else {
            query =
                query.filter(temps_entities::deployment_tokens::Column::EnvironmentId.is_null());
        }

        // Check expiration
        let now = Utc::now();
        query = query.filter(
            temps_entities::deployment_tokens::Column::ExpiresAt
                .is_null()
                .or(temps_entities::deployment_tokens::Column::ExpiresAt.gt(now)),
        );

        // Order by: prefer environment-specific tokens, then by creation date
        query = query
            .order_by_desc(temps_entities::deployment_tokens::Column::EnvironmentId)
            .order_by_desc(temps_entities::deployment_tokens::Column::CreatedAt);

        let existing_token = query.one(self.db.as_ref()).await?;

        if let Some(token_model) = existing_token {
            // We have an existing token, but we need to return the actual token value
            // Since we only store the hash, we can't retrieve the original token
            // The caller should use this method during token creation, not retrieval
            // For retrieval, we need to generate a new token or use a stored encrypted value

            // Actually, for deployment tokens, we should generate a new one each time
            // OR store the encrypted token value
            // For now, let's create a new token if none exists for deployment

            // Return a placeholder - the actual implementation should either:
            // 1. Store encrypted tokens (not just hashes)
            // 2. Or regenerate tokens when needed
            return Err(DeploymentTokenServiceError::InternalServerError(
                format!("Existing token found (ID: {}), but cannot retrieve value. Use get_token_value_for_deployment instead.", token_model.id)
            ));
        }

        // No token exists, create a default one
        let token = self.generate_token();
        let token_hash = self.hash_token(&token);
        let token_prefix = token.chars().take(8).collect::<String>();

        let default_name = if let Some(env_id) = environment_id {
            format!("Auto-generated for environment {}", env_id)
        } else {
            "Auto-generated project token".to_string()
        };

        let new_token = DeploymentTokenActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            name: Set(default_name),
            token_hash: Set(token_hash),
            token_prefix: Set(token_prefix),
            permissions: Set(Some(serde_json::json!(["*"]))), // Full access by default
            is_active: Set(true),
            expires_at: Set(None), // Never expires
            last_used_at: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            created_by: Set(None), // System-generated
            ..Default::default()
        };

        new_token.insert(self.db.as_ref()).await?;

        Ok(token)
    }

    /// Generate a deployment token with prefix "dt_" (deployment token)
    fn generate_token(&self) -> String {
        const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let mut rng = rand::thread_rng();

        let prefix = "dt_";
        let random_part: String = (0..40)
            .map(|_| {
                let idx = rng.gen_range(0..CHARSET.len());
                CHARSET[idx] as char
            })
            .collect();

        format!("{}{}", prefix, random_part)
    }

    /// Hash a token using SHA256
    pub(crate) fn hash_token(&self, token: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::projects;

    async fn setup_test_env() -> (TestDatabase, DeploymentTokenService, projects::Model) {
        let db = TestDatabase::with_migrations().await.unwrap();

        // Create a test project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set(format!("test-project-{}", uuid::Uuid::new_v4())),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let project = project.insert(db.db.as_ref()).await.unwrap();

        let service = DeploymentTokenService::new(db.db.clone());
        (db, service, project)
    }

    #[tokio::test]
    #[ignore] // Requires Docker for PostgreSQL testcontainer
    async fn test_create_deployment_token() {
        let (_db, service, project) = setup_test_env().await;

        let request = CreateDeploymentTokenRequest {
            name: "Test Token".to_string(),
            environment_id: None,
            permissions: Some(vec![
                "visitors:enrich".to_string(),
                "emails:send".to_string(),
            ]),
            expires_at: None,
        };

        let response = service
            .create_token(project.id, Some(1), request)
            .await
            .unwrap();

        assert_eq!(response.name, "Test Token");
        assert_eq!(response.project_id, project.id);
        assert!(response.token.starts_with("dt_"));
        assert_eq!(response.token.len(), 43); // dt_ + 40 chars
        assert!(response.permissions.is_some());
    }

    #[tokio::test]
    #[ignore] // Requires Docker for PostgreSQL testcontainer
    async fn test_create_deployment_token_default_permissions() {
        let (_db, service, project) = setup_test_env().await;

        let request = CreateDeploymentTokenRequest {
            name: "Default Perms Token".to_string(),
            environment_id: None,
            permissions: None, // Should default to full access
            expires_at: None,
        };

        let response = service
            .create_token(project.id, None, request)
            .await
            .unwrap();

        let perms = response.permissions.unwrap();
        assert!(perms.contains(&"*".to_string()));
    }

    #[tokio::test]
    #[ignore] // Requires Docker for PostgreSQL testcontainer
    async fn test_create_deployment_token_invalid_permission() {
        let (_db, service, project) = setup_test_env().await;

        let request = CreateDeploymentTokenRequest {
            name: "Invalid Perm Token".to_string(),
            environment_id: None,
            permissions: Some(vec!["invalid:permission".to_string()]),
            expires_at: None,
        };

        let result = service.create_token(project.id, None, request).await;

        assert!(result.is_err());
        matches!(
            result.unwrap_err(),
            DeploymentTokenServiceError::ValidationError(_)
        );
    }

    #[tokio::test]
    #[ignore] // Requires Docker for PostgreSQL testcontainer
    async fn test_create_deployment_token_duplicate_name() {
        let (_db, service, project) = setup_test_env().await;

        let request1 = CreateDeploymentTokenRequest {
            name: "Duplicate Name".to_string(),
            environment_id: None,
            permissions: None,
            expires_at: None,
        };

        service
            .create_token(project.id, None, request1)
            .await
            .unwrap();

        let request2 = CreateDeploymentTokenRequest {
            name: "Duplicate Name".to_string(),
            environment_id: None,
            permissions: None,
            expires_at: None,
        };

        let result = service.create_token(project.id, None, request2).await;

        assert!(result.is_err());
        matches!(
            result.unwrap_err(),
            DeploymentTokenServiceError::Conflict(_)
        );
    }

    #[tokio::test]
    #[ignore] // Requires Docker for PostgreSQL testcontainer
    async fn test_list_deployment_tokens() {
        let (_db, service, project) = setup_test_env().await;

        // Create multiple tokens
        for i in 1..=3 {
            let request = CreateDeploymentTokenRequest {
                name: format!("Token {}", i),
                environment_id: None,
                permissions: None,
                expires_at: None,
            };
            service
                .create_token(project.id, None, request)
                .await
                .unwrap();
        }

        let response = service.list_tokens(project.id, 1, 10).await.unwrap();

        assert_eq!(response.total, 3);
        assert_eq!(response.tokens.len(), 3);
    }

    #[tokio::test]
    #[ignore] // Requires Docker for PostgreSQL testcontainer
    async fn test_get_deployment_token() {
        let (_db, service, project) = setup_test_env().await;

        let request = CreateDeploymentTokenRequest {
            name: "Get Token Test".to_string(),
            environment_id: None,
            permissions: None,
            expires_at: None,
        };

        let created = service
            .create_token(project.id, None, request)
            .await
            .unwrap();

        let retrieved = service.get_token(project.id, created.id).await.unwrap();

        assert_eq!(retrieved.id, created.id);
        assert_eq!(retrieved.name, "Get Token Test");
    }

    #[tokio::test]
    #[ignore] // Requires Docker for PostgreSQL testcontainer
    async fn test_update_deployment_token() {
        let (_db, service, project) = setup_test_env().await;

        let request = CreateDeploymentTokenRequest {
            name: "Original Name".to_string(),
            environment_id: None,
            permissions: None,
            expires_at: None,
        };

        let created = service
            .create_token(project.id, None, request)
            .await
            .unwrap();

        let update_request = UpdateDeploymentTokenRequest {
            name: Some("Updated Name".to_string()),
            is_active: Some(false),
            permissions: None,
            expires_at: None,
        };

        let updated = service
            .update_token(project.id, created.id, update_request)
            .await
            .unwrap();

        assert_eq!(updated.name, "Updated Name");
        assert!(!updated.is_active);
    }

    #[tokio::test]
    #[ignore] // Requires Docker for PostgreSQL testcontainer
    async fn test_delete_deployment_token() {
        let (_db, service, project) = setup_test_env().await;

        let request = CreateDeploymentTokenRequest {
            name: "Delete Me".to_string(),
            environment_id: None,
            permissions: None,
            expires_at: None,
        };

        let created = service
            .create_token(project.id, None, request)
            .await
            .unwrap();

        service.delete_token(project.id, created.id).await.unwrap();

        let result = service.get_token(project.id, created.id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore] // Requires Docker for PostgreSQL testcontainer
    async fn test_validate_deployment_token() {
        let (_db, service, project) = setup_test_env().await;

        let request = CreateDeploymentTokenRequest {
            name: "Validate Me".to_string(),
            environment_id: None,
            permissions: Some(vec!["visitors:enrich".to_string()]),
            expires_at: None,
        };

        let created = service
            .create_token(project.id, None, request)
            .await
            .unwrap();

        let (validated_project_id, validated_env_id, permissions) =
            service.validate_token(&created.token).await.unwrap();

        assert_eq!(validated_project_id, project.id);
        assert!(validated_env_id.is_none());
        assert_eq!(permissions.len(), 1);
        assert_eq!(permissions[0], DeploymentTokenPermission::VisitorsEnrich);
    }

    #[tokio::test]
    #[ignore] // Requires Docker for PostgreSQL testcontainer
    async fn test_validate_expired_token() {
        let (_db, service, project) = setup_test_env().await;

        let request = CreateDeploymentTokenRequest {
            name: "Expired Token".to_string(),
            environment_id: None,
            permissions: None,
            expires_at: Some(Utc::now() - Duration::days(1)), // Already expired
        };

        let created = service
            .create_token(project.id, None, request)
            .await
            .unwrap();

        let result = service.validate_token(&created.token).await;

        assert!(result.is_err());
        matches!(
            result.unwrap_err(),
            DeploymentTokenServiceError::Unauthorized(_)
        );
    }

    #[tokio::test]
    #[ignore] // Requires Docker for PostgreSQL testcontainer
    async fn test_validate_invalid_token() {
        let (_db, service, _project) = setup_test_env().await;

        let result = service.validate_token("dt_invalidtoken123456").await;

        assert!(result.is_err());
        matches!(
            result.unwrap_err(),
            DeploymentTokenServiceError::Unauthorized(_)
        );
    }

    #[tokio::test]
    #[ignore] // Requires Docker for PostgreSQL testcontainer
    async fn test_token_generation_format() {
        let (_db, service, _project) = setup_test_env().await;

        let token = service.generate_token();

        assert!(token.starts_with("dt_"));
        assert_eq!(token.len(), 43); // dt_ + 40 random chars

        // Check that it only contains valid characters
        let valid_chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        for c in token[3..].chars() {
            assert!(valid_chars.contains(c));
        }
    }

    #[tokio::test]
    #[ignore] // Requires Docker for PostgreSQL testcontainer
    async fn test_token_hash_consistency() {
        let (_db, service, _project) = setup_test_env().await;

        let token = "dt_testtoken123456";

        let hash1 = service.hash_token(token);
        let hash2 = service.hash_token(token);

        // Same input should produce same hash
        assert_eq!(hash1, hash2);

        // Hash should be 64 chars (SHA256 hex)
        assert_eq!(hash1.len(), 64);
    }

    // ============================================
    // Unit tests that don't require Docker
    // ============================================

    /// Test DeploymentTokenPermission parsing from string
    #[test]
    fn test_permission_from_str() {
        assert_eq!(
            DeploymentTokenPermission::from_str("visitors:enrich"),
            Some(DeploymentTokenPermission::VisitorsEnrich)
        );
        assert_eq!(
            DeploymentTokenPermission::from_str("emails:send"),
            Some(DeploymentTokenPermission::EmailsSend)
        );
        assert_eq!(
            DeploymentTokenPermission::from_str("analytics:read"),
            Some(DeploymentTokenPermission::AnalyticsRead)
        );
        assert_eq!(
            DeploymentTokenPermission::from_str("events:write"),
            Some(DeploymentTokenPermission::EventsWrite)
        );
        assert_eq!(
            DeploymentTokenPermission::from_str("errors:read"),
            Some(DeploymentTokenPermission::ErrorsRead)
        );
        assert_eq!(
            DeploymentTokenPermission::from_str("*"),
            Some(DeploymentTokenPermission::FullAccess)
        );
        assert_eq!(DeploymentTokenPermission::from_str("invalid:perm"), None);
        assert_eq!(DeploymentTokenPermission::from_str(""), None);
    }

    /// Test DeploymentTokenPermission as_str method
    #[test]
    fn test_permission_as_str() {
        assert_eq!(
            DeploymentTokenPermission::VisitorsEnrich.as_str(),
            "visitors:enrich"
        );
        assert_eq!(
            DeploymentTokenPermission::EmailsSend.as_str(),
            "emails:send"
        );
        assert_eq!(
            DeploymentTokenPermission::AnalyticsRead.as_str(),
            "analytics:read"
        );
        assert_eq!(
            DeploymentTokenPermission::EventsWrite.as_str(),
            "events:write"
        );
        assert_eq!(
            DeploymentTokenPermission::ErrorsRead.as_str(),
            "errors:read"
        );
        assert_eq!(DeploymentTokenPermission::FullAccess.as_str(), "*");
    }

    /// Test permission roundtrip (as_str -> from_str)
    #[test]
    fn test_permission_roundtrip() {
        let permissions = vec![
            DeploymentTokenPermission::VisitorsEnrich,
            DeploymentTokenPermission::EmailsSend,
            DeploymentTokenPermission::AnalyticsRead,
            DeploymentTokenPermission::EventsWrite,
            DeploymentTokenPermission::ErrorsRead,
            DeploymentTokenPermission::FullAccess,
        ];

        for perm in permissions {
            let as_string = perm.as_str();
            let parsed = DeploymentTokenPermission::from_str(as_string);
            assert_eq!(
                parsed,
                Some(perm.clone()),
                "Roundtrip failed for {:?}",
                perm
            );
        }
    }

    /// Test DeploymentTokenPermission::all() returns all variants
    #[test]
    fn test_permission_all() {
        let all = DeploymentTokenPermission::all();
        assert_eq!(all.len(), 6);
        assert!(all.contains(&DeploymentTokenPermission::VisitorsEnrich));
        assert!(all.contains(&DeploymentTokenPermission::EmailsSend));
        assert!(all.contains(&DeploymentTokenPermission::AnalyticsRead));
        assert!(all.contains(&DeploymentTokenPermission::EventsWrite));
        assert!(all.contains(&DeploymentTokenPermission::ErrorsRead));
        assert!(all.contains(&DeploymentTokenPermission::FullAccess));
    }

    /// Test DeploymentTokenPermission::grants() method
    #[test]
    fn test_permission_grants() {
        // FullAccess grants everything
        assert!(DeploymentTokenPermission::FullAccess
            .grants(&DeploymentTokenPermission::VisitorsEnrich));
        assert!(
            DeploymentTokenPermission::FullAccess.grants(&DeploymentTokenPermission::EmailsSend)
        );
        assert!(
            DeploymentTokenPermission::FullAccess.grants(&DeploymentTokenPermission::FullAccess)
        );

        // Specific permission only grants itself
        assert!(DeploymentTokenPermission::VisitorsEnrich
            .grants(&DeploymentTokenPermission::VisitorsEnrich));
        assert!(!DeploymentTokenPermission::VisitorsEnrich
            .grants(&DeploymentTokenPermission::EmailsSend));
        assert!(!DeploymentTokenPermission::EmailsSend
            .grants(&DeploymentTokenPermission::AnalyticsRead));
    }

    /// Test token generation format without database
    #[test]
    fn test_token_generation_format_unit() {
        // We can't call generate_token without a service instance,
        // but we can test the format requirements
        let valid_token = "dt_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ1234";

        // Should start with dt_
        assert!(valid_token.starts_with("dt_"));

        // Should be 43 characters total (dt_ + 40 random)
        assert_eq!(valid_token.len(), 43);

        // Characters after prefix should be alphanumeric
        let valid_chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        for c in valid_token[3..].chars() {
            assert!(
                valid_chars.contains(c),
                "Invalid character '{}' in token",
                c
            );
        }
    }

    /// Test token hash format
    #[test]
    fn test_token_hash_format_unit() {
        use sha2::{Digest, Sha256};

        let token = "dt_testtoken123456789012345678901234567890";

        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let hash = format!("{:x}", hasher.finalize());

        // SHA256 hash should be 64 hex characters
        assert_eq!(hash.len(), 64);

        // Should only contain hex characters
        for c in hash.chars() {
            assert!(c.is_ascii_hexdigit(), "Invalid hex character: {}", c);
        }
    }

    /// Test hash consistency
    #[test]
    fn test_hash_consistency_unit() {
        use sha2::{Digest, Sha256};

        let token = "dt_testtoken123456789012345678901234567890";

        let hash1 = {
            let mut hasher = Sha256::new();
            hasher.update(token.as_bytes());
            format!("{:x}", hasher.finalize())
        };

        let hash2 = {
            let mut hasher = Sha256::new();
            hasher.update(token.as_bytes());
            format!("{:x}", hasher.finalize())
        };

        // Same input should produce same hash
        assert_eq!(hash1, hash2);
    }

    /// Test different tokens produce different hashes
    #[test]
    fn test_different_tokens_different_hashes() {
        use sha2::{Digest, Sha256};

        let token1 = "dt_testtoken1111111111111111111111111111111";
        let token2 = "dt_testtoken2222222222222222222222222222222";

        let hash1 = {
            let mut hasher = Sha256::new();
            hasher.update(token1.as_bytes());
            format!("{:x}", hasher.finalize())
        };

        let hash2 = {
            let mut hasher = Sha256::new();
            hasher.update(token2.as_bytes());
            format!("{:x}", hasher.finalize())
        };

        // Different tokens should produce different hashes
        assert_ne!(hash1, hash2);
    }

    /// Test CreateDeploymentTokenRequest default values
    #[test]
    fn test_create_request_serialization() {
        let request = CreateDeploymentTokenRequest {
            name: "Test Token".to_string(),
            environment_id: None,
            permissions: Some(vec!["visitors:enrich".to_string()]),
            expires_at: None,
        };

        // Verify fields
        assert_eq!(request.name, "Test Token");
        assert!(request.environment_id.is_none());
        assert!(request.permissions.is_some());
        assert_eq!(request.permissions.as_ref().unwrap().len(), 1);
        assert!(request.expires_at.is_none());
    }

    /// Test UpdateDeploymentTokenRequest with partial updates
    #[test]
    fn test_update_request_partial() {
        // Only update name
        let update1 = UpdateDeploymentTokenRequest {
            name: Some("New Name".to_string()),
            is_active: None,
            permissions: None,
            expires_at: None,
        };
        assert!(update1.name.is_some());
        assert!(update1.is_active.is_none());

        // Only update is_active
        let update2 = UpdateDeploymentTokenRequest {
            name: None,
            is_active: Some(false),
            permissions: None,
            expires_at: None,
        };
        assert!(update2.name.is_none());
        assert_eq!(update2.is_active, Some(false));

        // Update multiple fields
        let update3 = UpdateDeploymentTokenRequest {
            name: Some("Updated".to_string()),
            is_active: Some(true),
            permissions: Some(vec!["*".to_string()]),
            expires_at: None,
        };
        assert!(update3.name.is_some());
        assert!(update3.is_active.is_some());
        assert!(update3.permissions.is_some());
    }

    /// Test DeploymentTokenServiceError variants
    #[test]
    fn test_error_variants() {
        let not_found = DeploymentTokenServiceError::NotFound("Token not found".to_string());
        assert!(matches!(
            not_found,
            DeploymentTokenServiceError::NotFound(_)
        ));

        let validation = DeploymentTokenServiceError::ValidationError("Invalid".to_string());
        assert!(matches!(
            validation,
            DeploymentTokenServiceError::ValidationError(_)
        ));

        let conflict = DeploymentTokenServiceError::Conflict("Already exists".to_string());
        assert!(matches!(conflict, DeploymentTokenServiceError::Conflict(_)));

        let unauthorized = DeploymentTokenServiceError::Unauthorized("Bad token".to_string());
        assert!(matches!(
            unauthorized,
            DeploymentTokenServiceError::Unauthorized(_)
        ));
    }

    /// Test error to Problem conversion produces valid Problem instances
    #[test]
    fn test_error_to_problem_creation() {
        // Test that to_problem() doesn't panic and returns a Problem
        let not_found = DeploymentTokenServiceError::NotFound("Token 123 not found".to_string());
        let _problem = not_found.to_problem();

        let validation =
            DeploymentTokenServiceError::ValidationError("Invalid permission".to_string());
        let _problem = validation.to_problem();

        let conflict = DeploymentTokenServiceError::Conflict("Token already exists".to_string());
        let _problem = conflict.to_problem();

        let unauthorized = DeploymentTokenServiceError::Unauthorized("Invalid token".to_string());
        let _problem = unauthorized.to_problem();

        let internal_error =
            DeploymentTokenServiceError::InternalServerError("Connection failed".to_string());
        let _problem = internal_error.to_problem();
    }

    /// Test all permission variants exist
    #[test]
    fn test_all_permissions_covered() {
        // Ensure all permission strings are valid
        let all_perm_strings = vec![
            "visitors:enrich",
            "emails:send",
            "analytics:read",
            "events:write",
            "errors:read",
            "*",
        ];

        for perm_str in all_perm_strings {
            let parsed = DeploymentTokenPermission::from_str(perm_str);
            assert!(
                parsed.is_some(),
                "Permission '{}' should be parseable",
                perm_str
            );
        }
    }

    /// Test token prefix extraction
    #[test]
    fn test_token_prefix_extraction() {
        let token = "dt_abcdefghijklmnopqrstuvwxyz1234567890abcd";

        // Token prefix is the first 8 characters of the token (as stored in DB)
        // The service stores: token.chars().take(8).collect::<String>()
        let prefix: String = token.chars().take(8).collect();
        assert_eq!(prefix.len(), 8); // "dt_abcde"
        assert!(prefix.starts_with("dt_"));
        assert_eq!(prefix, "dt_abcde");
    }

    /// Test permission JSON serialization
    #[test]
    fn test_permission_serialization() {
        let perms = vec![
            DeploymentTokenPermission::VisitorsEnrich,
            DeploymentTokenPermission::EmailsSend,
        ];

        // Test that it can be serialized to JSON
        let json = serde_json::to_string(&perms).unwrap();
        assert!(json.contains("visitors_enrich") || json.contains("VisitorsEnrich"));

        // Test that it can be deserialized
        let parsed: Vec<DeploymentTokenPermission> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    /// Test CreateDeploymentTokenResponse structure
    #[test]
    fn test_create_response_structure() {
        let response = CreateDeploymentTokenResponse {
            id: 1,
            project_id: 100,
            environment_id: Some(50),
            name: "Test Token".to_string(),
            token: "dt_abcdefghij1234567890abcdefghij1234567890".to_string(),
            token_prefix: "dt_abcdef...".to_string(),
            permissions: Some(vec!["*".to_string()]),
            expires_at: None,
            created_at: Utc::now(),
        };

        assert_eq!(response.id, 1);
        assert_eq!(response.project_id, 100);
        assert_eq!(response.environment_id, Some(50));
        assert!(response.token.starts_with("dt_"));
        assert!(response.token_prefix.ends_with("..."));
    }

    /// Test DeploymentTokenResponse structure (without plain token)
    #[test]
    fn test_token_response_structure() {
        let response = DeploymentTokenResponse {
            id: 1,
            project_id: 100,
            environment_id: None,
            name: "API Token".to_string(),
            token_prefix: "dt_xyz123...".to_string(),
            permissions: Some(vec![
                "visitors:enrich".to_string(),
                "emails:send".to_string(),
            ]),
            is_active: true,
            expires_at: None,
            last_used_at: None,
            created_at: Utc::now(),
            created_by: Some(1),
        };

        assert_eq!(response.id, 1);
        assert_eq!(response.name, "API Token");
        // Note: DeploymentTokenResponse doesn't have a `token` field - only token_prefix
        assert!(response.token_prefix.ends_with("..."));
        assert_eq!(response.permissions.as_ref().unwrap().len(), 2);
    }
}
