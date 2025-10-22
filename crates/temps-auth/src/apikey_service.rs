use crate::permissions::{Permission, Role};
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
use temps_entities::api_keys::{ActiveModel as ApiKeyActiveModel, Entity as ApiKeyEntity};
use temps_entities::users;
use thiserror::Error;

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use utoipa::ToSchema;

// Response DTOs
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiKeyResponse {
    pub id: i32,
    pub name: String,
    pub key_prefix: String,
    pub role_type: String,
    pub permissions: Option<Vec<String>>, // Reserved for future use
    pub is_active: bool,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub last_used_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub created_at: UtcDateTime,
}

impl From<temps_entities::api_keys::Model> for ApiKeyResponse {
    fn from(model: temps_entities::api_keys::Model) -> Self {
        Self {
            id: model.id,
            name: model.name,
            key_prefix: model.key_prefix,
            role_type: model.role_type,
            permissions: model
                .permissions
                .and_then(|p| serde_json::from_str(&p).ok()),
            is_active: model.is_active,
            expires_at: model.expires_at,
            last_used_at: model.last_used_at,
            created_at: model.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateApiKeyResponse {
    pub id: i32,
    pub name: String,
    pub key_prefix: String,
    pub role_type: String,
    pub permissions: Option<Vec<String>>, // Reserved for future use
    pub api_key: String,                  // Only returned on creation
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub created_at: UtcDateTime,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiKeyListResponse {
    pub api_keys: Vec<ApiKeyResponse>,
    pub total: u64,
}

// Request DTOs
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateApiKeyRequest {
    pub name: String,
    #[schema(example = "admin")]
    pub role_type: String,
    #[schema(example = json!(["projects:read", "deployments:read"]))]
    pub permissions: Option<Vec<String>>,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateApiKeyRequest {
    pub name: Option<String>,
    pub is_active: Option<bool>,
    #[schema(example = json!(["projects:read", "deployments:read"]))]
    pub permissions: Option<Vec<String>>,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
}

#[derive(Error, Debug)]
pub enum ApiKeyServiceError {
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

impl ApiKeyServiceError {
    pub fn to_problem(&self) -> Problem {
        match self {
            ApiKeyServiceError::DatabaseError(e) => {
                ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .type_("https://temps.sh/probs/database-error")
                    .title("Database Error")
                    .detail(format!("A database error occurred: {}", e))
                    .value("error_code", "DATABASE_ERROR")
                    .build()
            }
            ApiKeyServiceError::NotFound(msg) => ErrorBuilder::new(StatusCode::NOT_FOUND)
                .type_("https://temps.sh/probs/api-key-not-found")
                .title("API Key Not Found")
                .detail(msg.clone())
                .value("error_code", "API_KEY_NOT_FOUND")
                .build(),
            ApiKeyServiceError::ValidationError(msg) => ErrorBuilder::new(StatusCode::BAD_REQUEST)
                .type_("https://temps.sh/probs/validation-error")
                .title("Validation Error")
                .detail(msg.clone())
                .value("error_code", "VALIDATION_ERROR")
                .build(),
            ApiKeyServiceError::Unauthorized(msg) => ErrorBuilder::new(StatusCode::UNAUTHORIZED)
                .type_("https://temps.sh/probs/unauthorized")
                .title("Unauthorized")
                .detail(msg.clone())
                .value("error_code", "UNAUTHORIZED")
                .build(),
            ApiKeyServiceError::Conflict(msg) => ErrorBuilder::new(StatusCode::CONFLICT)
                .type_("https://temps.sh/probs/conflict")
                .title("Conflict")
                .detail(msg.clone())
                .value("error_code", "CONFLICT")
                .build(),
            ApiKeyServiceError::InternalServerError(msg) => {
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

pub struct ApiKeyService {
    db: Arc<DbConnection>,
}

impl ApiKeyService {
    pub fn new(db: Arc<DbConnection>) -> Self {
        Self { db }
    }

    pub async fn create_api_key(
        &self,
        user_id: i32,
        request: CreateApiKeyRequest,
    ) -> Result<CreateApiKeyResponse, ApiKeyServiceError> {
        // Validate role type and permissions
        let permissions_json = if request.role_type == "custom" {
            // For custom role, validate that permissions are provided
            if request.permissions.is_none() || request.permissions.as_ref().unwrap().is_empty() {
                return Err(ApiKeyServiceError::ValidationError(
                    "Custom role requires at least one permission".to_string(),
                ));
            }

            // Validate each permission
            let permissions = request.permissions.as_ref().unwrap();
            for perm_str in permissions {
                if Permission::from_str(perm_str).is_none() {
                    return Err(ApiKeyServiceError::ValidationError(format!(
                        "Invalid permission: {}",
                        perm_str
                    )));
                }
            }

            // Store permissions as JSON string
            Some(serde_json::to_string(&permissions).unwrap())
        } else {
            // For predefined roles, validate the role exists
            if Role::from_str(&request.role_type).is_none() {
                return Err(ApiKeyServiceError::ValidationError(
                    format!("Invalid role type: {}. Valid roles are: admin, user, reader, mcp, api_reader, or custom", request.role_type)
                ));
            }
            None
        };

        // Check if name is unique for this user
        let existing_key = ApiKeyEntity::find()
            .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
            .filter(temps_entities::api_keys::Column::Name.eq(&request.name))
            .one(self.db.as_ref())
            .await?;

        if existing_key.is_some() {
            return Err(ApiKeyServiceError::Conflict(
                "API key with this name already exists".to_string(),
            ));
        }

        // Generate API key
        let api_key = self.generate_api_key();
        let key_hash = self.hash_api_key(&api_key);
        let key_prefix = api_key.chars().take(8).collect::<String>();

        let now = Utc::now();
        let expires_at = request.expires_at.or_else(|| {
            // Default expiration: 1 year from now
            Some(now + chrono::Duration::days(365))
        });

        let new_api_key = ApiKeyActiveModel {
            name: Set(request.name.clone()),
            key_hash: Set(key_hash),
            key_prefix: Set(key_prefix.clone()),
            user_id: Set(user_id),
            role_type: Set(request.role_type.clone()),
            permissions: Set(permissions_json),
            is_active: Set(true),
            expires_at: Set(expires_at),
            last_used_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };

        let api_key_model = new_api_key.insert(self.db.as_ref()).await?;

        Ok(CreateApiKeyResponse {
            id: api_key_model.id,
            name: api_key_model.name,
            key_prefix,
            role_type: api_key_model.role_type,
            permissions: api_key_model
                .permissions
                .and_then(|p| serde_json::from_str(&p).ok()),
            api_key, // Only returned on creation
            expires_at: api_key_model.expires_at,
            created_at: api_key_model.created_at,
        })
    }

    pub async fn list_api_keys(
        &self,
        user_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<ApiKeyListResponse, ApiKeyServiceError> {
        let paginator = ApiKeyEntity::find()
            .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
            .order_by_desc(temps_entities::api_keys::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);

        let total = paginator.num_items().await?;
        let api_keys_models = paginator.fetch_page(page.saturating_sub(1)).await?;

        let api_keys = api_keys_models
            .into_iter()
            .map(ApiKeyResponse::from)
            .collect();

        Ok(ApiKeyListResponse { api_keys, total })
    }

    pub async fn get_api_key(
        &self,
        user_id: i32,
        api_key_id: i32,
    ) -> Result<ApiKeyResponse, ApiKeyServiceError> {
        let api_key = ApiKeyEntity::find_by_id(api_key_id)
            .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApiKeyServiceError::NotFound("API key not found".to_string()))?;

        Ok(ApiKeyResponse::from(api_key))
    }

    pub async fn update_api_key(
        &self,
        user_id: i32,
        api_key_id: i32,
        request: UpdateApiKeyRequest,
    ) -> Result<ApiKeyResponse, ApiKeyServiceError> {
        let api_key = ApiKeyEntity::find_by_id(api_key_id)
            .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApiKeyServiceError::NotFound("API key not found".to_string()))?;

        // Check if new name conflicts with existing keys
        if let Some(ref new_name) = request.name {
            if new_name != &api_key.name {
                let existing_key = ApiKeyEntity::find()
                    .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
                    .filter(temps_entities::api_keys::Column::Name.eq(new_name))
                    .filter(temps_entities::api_keys::Column::Id.ne(api_key_id))
                    .one(self.db.as_ref())
                    .await?;

                if existing_key.is_some() {
                    return Err(ApiKeyServiceError::Conflict(
                        "API key with this name already exists".to_string(),
                    ));
                }
            }
        }

        let mut api_key_active: ApiKeyActiveModel = api_key.into();

        if let Some(name) = request.name {
            api_key_active.name = Set(name);
        }
        if let Some(is_active) = request.is_active {
            api_key_active.is_active = Set(is_active);
        }
        if let Some(expires_at) = request.expires_at {
            api_key_active.expires_at = Set(Some(expires_at));
        }
        // Note: We no longer support custom permissions, only standard roles
        api_key_active.updated_at = Set(Utc::now());

        let updated_api_key = api_key_active.update(self.db.as_ref()).await?;

        Ok(ApiKeyResponse::from(updated_api_key))
    }

    pub async fn delete_api_key(
        &self,
        user_id: i32,
        api_key_id: i32,
    ) -> Result<(), ApiKeyServiceError> {
        let api_key = ApiKeyEntity::find_by_id(api_key_id)
            .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApiKeyServiceError::NotFound("API key not found".to_string()))?;

        ApiKeyEntity::delete_by_id(api_key.id)
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }

    pub async fn validate_api_key(
        &self,
        api_key: &str,
    ) -> Result<
        (
            users::Model,
            Option<Role>,
            Option<Vec<Permission>>,
            String,
            i32,
        ),
        ApiKeyServiceError,
    > {
        let key_hash = self.hash_api_key(api_key);
        let key_prefix = api_key.chars().take(8).collect::<String>();

        let api_key_model = ApiKeyEntity::find()
            .filter(temps_entities::api_keys::Column::KeyHash.eq(&key_hash))
            .filter(temps_entities::api_keys::Column::KeyPrefix.eq(&key_prefix))
            .filter(temps_entities::api_keys::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApiKeyServiceError::Unauthorized("Invalid API key".to_string()))?;

        // Check if expired
        if let Some(expires_at) = api_key_model.expires_at {
            if expires_at <= Utc::now() {
                return Err(ApiKeyServiceError::Unauthorized(
                    "API key has expired".to_string(),
                ));
            }
        }

        // Get user
        let user = users::Entity::find_by_id(api_key_model.user_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApiKeyServiceError::NotFound("User not found".to_string()))?;

        // Parse role or permissions based on role_type
        let (role, permissions) = if api_key_model.role_type == "custom" {
            // For custom role, parse permissions from JSON
            let perms = if let Some(perms_json) = &api_key_model.permissions {
                let perm_strings: Vec<String> = serde_json::from_str(perms_json).map_err(|_| {
                    ApiKeyServiceError::InternalServerError(
                        "Invalid permissions in database".to_string(),
                    )
                })?;

                let mut permissions = Vec::new();
                for perm_str in perm_strings {
                    if let Some(perm) = Permission::from_str(&perm_str) {
                        permissions.push(perm);
                    }
                }
                Some(permissions)
            } else {
                return Err(ApiKeyServiceError::InternalServerError(
                    "Custom role but no permissions defined".to_string(),
                ));
            };
            (None, perms)
        } else {
            // For predefined roles
            let role = Role::from_str(&api_key_model.role_type).ok_or_else(|| {
                ApiKeyServiceError::InternalServerError("Invalid role type in database".to_string())
            })?;
            (Some(role), None)
        };

        // Update last_used_at
        let mut api_key_active: ApiKeyActiveModel = api_key_model.clone().into();
        api_key_active.last_used_at = Set(Some(Utc::now()));
        let _ = api_key_active.update(self.db.as_ref()).await; // Don't fail if this fails

        Ok((
            user,
            role,
            permissions,
            api_key_model.name,
            api_key_model.id,
        ))
    }

    pub async fn deactivate_api_key(
        &self,
        user_id: i32,
        api_key_id: i32,
    ) -> Result<ApiKeyResponse, ApiKeyServiceError> {
        let request = UpdateApiKeyRequest {
            name: None,
            is_active: Some(false),
            permissions: None,
            expires_at: None,
        };
        self.update_api_key(user_id, api_key_id, request).await
    }

    pub async fn activate_api_key(
        &self,
        user_id: i32,
        api_key_id: i32,
    ) -> Result<ApiKeyResponse, ApiKeyServiceError> {
        let request = UpdateApiKeyRequest {
            name: None,
            is_active: Some(true),
            permissions: None,
            expires_at: None,
        };
        self.update_api_key(user_id, api_key_id, request).await
    }

    fn generate_api_key(&self) -> String {
        const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let mut rng = rand::thread_rng();

        let prefix = "tk_";
        let random_part: String = (0..40)
            .map(|_| {
                let idx = rng.gen_range(0..CHARSET.len());
                CHARSET[idx] as char
            })
            .collect();

        format!("{}{}", prefix, random_part)
    }

    pub(crate) fn hash_api_key(&self, api_key: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(api_key.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use sea_orm::{ActiveModelTrait, Set};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{api_keys, users};

    async fn setup_test_env() -> (TestDatabase, ApiKeyService, users::Model) {
        let db = TestDatabase::with_migrations().await.unwrap();

        // Generate unique email to avoid conflicts in parallel tests
        let test_id = uuid::Uuid::new_v4();
        let email = format!("test_{}@example.com", test_id);

        // Create a test user with unique email
        let user = users::ActiveModel {
            email: Set(email),
            name: Set("Test User".to_string()),
            email_verified: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            mfa_enabled: Set(false),
            ..Default::default()
        };
        let user = user.insert(db.db.as_ref()).await.unwrap();

        let api_key_service = ApiKeyService::new(db.db.clone());
        (db, api_key_service, user)
    }

    async fn create_test_api_key(
        db: &Arc<DbConnection>,
        user_id: i32,
        name: &str,
        role_type: &str,
    ) -> api_keys::Model {
        // Generate unique key_hash and key_prefix to avoid conflicts
        let unique_id = uuid::Uuid::new_v4();
        let api_key = api_keys::ActiveModel {
            name: Set(name.to_string()),
            key_hash: Set(format!("test_hash_{}", unique_id)),
            key_prefix: Set(format!("tk_{}", &unique_id.to_string()[..8])),
            user_id: Set(user_id),
            role_type: Set(role_type.to_string()),
            permissions: Set(None),
            is_active: Set(true),
            expires_at: Set(Some((Utc::now() + Duration::days(365)))),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        api_key.insert(db.as_ref()).await.unwrap()
    }

    // API Key Creation Tests

    #[tokio::test]
    async fn test_create_api_key_with_admin_role() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            name: "Admin API Key".to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: Some(Utc::now() + Duration::days(30)),
        };

        let response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        assert_eq!(response.name, "Admin API Key");
        assert_eq!(response.role_type, "admin");
        assert!(response.api_key.starts_with("tk_"));
        assert_eq!(response.api_key.len(), 43); // tk_ + 40 chars
        assert!(response.permissions.is_none());
    }

    #[tokio::test]
    async fn test_create_api_key_with_custom_role() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            name: "Custom API Key".to_string(),
            role_type: "custom".to_string(),
            permissions: Some(vec![
                "projects:read".to_string(),
                "deployments:read".to_string(),
            ]),
            expires_at: None,
        };

        let response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        assert_eq!(response.name, "Custom API Key");
        assert_eq!(response.role_type, "custom");
        assert!(response.permissions.is_some());
        let perms = response.permissions.unwrap();
        assert_eq!(perms.len(), 2);
        assert!(perms.contains(&"projects:read".to_string()));
    }

    #[tokio::test]
    async fn test_create_api_key_custom_without_permissions_fails() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            name: "Invalid Custom Key".to_string(),
            role_type: "custom".to_string(),
            permissions: None,
            expires_at: None,
        };

        let result = api_key_service.create_api_key(user.id, request).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::ValidationError(_));
    }

    #[tokio::test]
    async fn test_create_api_key_with_invalid_role() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            name: "Invalid Role Key".to_string(),
            role_type: "invalid_role".to_string(),
            permissions: None,
            expires_at: None,
        };

        let result = api_key_service.create_api_key(user.id, request).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::ValidationError(_));
    }

    #[tokio::test]
    async fn test_create_api_key_duplicate_name_fails() {
        let (_db, api_key_service, user) = setup_test_env().await;

        // Create first key
        let request1 = CreateApiKeyRequest {
            name: "Duplicate Name".to_string(),
            role_type: "reader".to_string(),
            permissions: None,
            expires_at: None,
        };

        api_key_service
            .create_api_key(user.id, request1)
            .await
            .unwrap();

        // Try to create second key with same name
        let request2 = CreateApiKeyRequest {
            name: "Duplicate Name".to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: None,
        };

        let result = api_key_service.create_api_key(user.id, request2).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::Conflict(_));
    }

    #[tokio::test]
    async fn test_create_api_key_with_invalid_permission() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            name: "Invalid Permission Key".to_string(),
            role_type: "custom".to_string(),
            permissions: Some(vec!["invalid:permission".to_string()]),
            expires_at: None,
        };

        let result = api_key_service.create_api_key(user.id, request).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::ValidationError(_));
    }

    // API Key Listing Tests

    #[tokio::test]
    async fn test_list_api_keys() {
        let (db, api_key_service, user) = setup_test_env().await;

        // Create multiple API keys
        create_test_api_key(&db.db, user.id, "Key 1", "admin").await;
        create_test_api_key(&db.db, user.id, "Key 2", "reader").await;
        create_test_api_key(&db.db, user.id, "Key 3", "user").await;

        let response = api_key_service.list_api_keys(user.id, 1, 10).await.unwrap();

        assert_eq!(response.total, 3);
        assert_eq!(response.api_keys.len(), 3);
        // Should be ordered by created_at DESC
        assert_eq!(response.api_keys[0].name, "Key 3");
        assert_eq!(response.api_keys[1].name, "Key 2");
        assert_eq!(response.api_keys[2].name, "Key 1");
    }

    #[tokio::test]
    async fn test_list_api_keys_pagination() {
        let (db, api_key_service, user) = setup_test_env().await;

        // Create 5 API keys
        for i in 1..=5 {
            create_test_api_key(&db.db, user.id, &format!("Key {}", i), "reader").await;
        }

        // Get first page
        let page1 = api_key_service.list_api_keys(user.id, 1, 2).await.unwrap();
        assert_eq!(page1.total, 5);
        assert_eq!(page1.api_keys.len(), 2);

        // Get second page
        let page2 = api_key_service.list_api_keys(user.id, 2, 2).await.unwrap();
        assert_eq!(page2.total, 5);
        assert_eq!(page2.api_keys.len(), 2);

        // Get third page
        let page3 = api_key_service.list_api_keys(user.id, 3, 2).await.unwrap();
        assert_eq!(page3.total, 5);
        assert_eq!(page3.api_keys.len(), 1);
    }

    #[tokio::test]
    async fn test_list_api_keys_empty() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let response = api_key_service.list_api_keys(user.id, 1, 10).await.unwrap();

        assert_eq!(response.total, 0);
        assert_eq!(response.api_keys.len(), 0);
    }

    // API Key Retrieval Tests

    #[tokio::test]
    async fn test_get_api_key() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        let response = api_key_service
            .get_api_key(user.id, api_key.id)
            .await
            .unwrap();

        assert_eq!(response.id, api_key.id);
        assert_eq!(response.name, "Test Key");
        assert_eq!(response.role_type, "admin");
    }

    #[tokio::test]
    async fn test_get_api_key_not_found() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let result = api_key_service.get_api_key(user.id, 999999).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
    }

    #[tokio::test]
    async fn test_get_api_key_wrong_user() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        // Try to get key with different user ID
        let result = api_key_service.get_api_key(user.id + 1, api_key.id).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
    }

    // API Key Update Tests

    #[tokio::test]
    async fn test_update_api_key_name() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Old Name", "admin").await;

        let request = UpdateApiKeyRequest {
            name: Some("New Name".to_string()),
            is_active: None,
            permissions: None,
            expires_at: None,
        };

        let response = api_key_service
            .update_api_key(user.id, api_key.id, request)
            .await
            .unwrap();

        assert_eq!(response.name, "New Name");
        assert!(response.is_active);
    }

    #[tokio::test]
    async fn test_update_api_key_deactivate() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        let request = UpdateApiKeyRequest {
            name: None,
            is_active: Some(false),
            permissions: None,
            expires_at: None,
        };

        let response = api_key_service
            .update_api_key(user.id, api_key.id, request)
            .await
            .unwrap();

        assert!(!response.is_active);
    }

    #[tokio::test]
    async fn test_update_api_key_expiration() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        let new_expiry = Utc::now() + Duration::days(7);
        let request = UpdateApiKeyRequest {
            name: None,
            is_active: None,
            permissions: None,
            expires_at: Some(new_expiry),
        };

        let response = api_key_service
            .update_api_key(user.id, api_key.id, request)
            .await
            .unwrap();

        assert!(response.expires_at.is_some());
        let expires_at = response.expires_at.unwrap();
        assert!((expires_at.timestamp() - new_expiry.timestamp()).abs() < 2);
    }

    #[tokio::test]
    async fn test_update_api_key_duplicate_name_fails() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key1 = create_test_api_key(&db.db, user.id, "Key 1", "admin").await;
        let _api_key2 = create_test_api_key(&db.db, user.id, "Key 2", "reader").await;

        let request = UpdateApiKeyRequest {
            name: Some("Key 2".to_string()), // Try to rename to existing name
            is_active: None,
            permissions: None,
            expires_at: None,
        };

        let result = api_key_service
            .update_api_key(user.id, api_key1.id, request)
            .await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::Conflict(_));
    }

    #[tokio::test]
    async fn test_update_api_key_not_found() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = UpdateApiKeyRequest {
            name: Some("New Name".to_string()),
            is_active: None,
            permissions: None,
            expires_at: None,
        };

        let result = api_key_service
            .update_api_key(user.id, 999999, request)
            .await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
    }

    // API Key Deletion Tests

    #[tokio::test]
    async fn test_delete_api_key() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        api_key_service
            .delete_api_key(user.id, api_key.id)
            .await
            .unwrap();

        // Verify key is deleted
        let result = api_key_service.get_api_key(user.id, api_key.id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_delete_api_key_not_found() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let result = api_key_service.delete_api_key(user.id, 999999).await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
    }

    #[tokio::test]
    async fn test_delete_api_key_wrong_user() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        let result = api_key_service
            .delete_api_key(user.id + 1, api_key.id)
            .await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::NotFound(_));
    }

    // API Key Validation Tests

    #[tokio::test]
    async fn test_validate_api_key_success() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            name: "Valid Key".to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: Some(Utc::now() + Duration::days(30)),
        };

        let create_response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        let (validated_user, role, permissions, key_name, key_id) = api_key_service
            .validate_api_key(&create_response.api_key)
            .await
            .unwrap();

        assert_eq!(validated_user.id, user.id);
        assert!(role.is_some());
        assert_eq!(role.unwrap(), Role::Admin);
        assert!(permissions.is_none());
        assert_eq!(key_name, "Valid Key");
        assert_eq!(key_id, create_response.id);
    }

    #[tokio::test]
    async fn test_validate_api_key_custom_permissions() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            name: "Custom Key".to_string(),
            role_type: "custom".to_string(),
            permissions: Some(vec!["projects:read".to_string()]),
            expires_at: None,
        };

        let create_response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        let (validated_user, role, permissions, _, _) = api_key_service
            .validate_api_key(&create_response.api_key)
            .await
            .unwrap();

        assert_eq!(validated_user.id, user.id);
        assert!(role.is_none());
        assert!(permissions.is_some());
        let perms = permissions.unwrap();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0], Permission::ProjectsRead);
    }

    #[tokio::test]
    async fn test_validate_api_key_invalid() {
        let (_db, api_key_service, _user) = setup_test_env().await;

        let result = api_key_service
            .validate_api_key("tk_invalidkey123456")
            .await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::Unauthorized(_));
    }

    #[tokio::test]
    async fn test_validate_api_key_expired() {
        let (_db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            name: "Expired Key".to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: Some(Utc::now() - Duration::days(1)), // Already expired
        };

        let create_response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        let result = api_key_service
            .validate_api_key(&create_response.api_key)
            .await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::Unauthorized(_));
    }

    #[tokio::test]
    async fn test_validate_api_key_inactive() {
        let (_db, api_key_service, user) = setup_test_env().await;

        // Create and then deactivate key
        let request = CreateApiKeyRequest {
            name: "Inactive Key".to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: None,
        };

        let create_response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        api_key_service
            .deactivate_api_key(user.id, create_response.id)
            .await
            .unwrap();

        let result = api_key_service
            .validate_api_key(&create_response.api_key)
            .await;

        assert!(result.is_err());
        matches!(result.unwrap_err(), ApiKeyServiceError::Unauthorized(_));
    }

    #[tokio::test]
    async fn test_validate_api_key_updates_last_used() {
        let (db, api_key_service, user) = setup_test_env().await;

        let request = CreateApiKeyRequest {
            name: "Track Usage Key".to_string(),
            role_type: "admin".to_string(),
            permissions: None,
            expires_at: None,
        };

        let create_response = api_key_service
            .create_api_key(user.id, request)
            .await
            .unwrap();

        // Validate the key
        api_key_service
            .validate_api_key(&create_response.api_key)
            .await
            .unwrap();

        // Check that last_used_at was updated
        let api_key = api_keys::Entity::find_by_id(create_response.id)
            .one(db.db.as_ref())
            .await
            .unwrap()
            .unwrap();

        assert!(api_key.last_used_at.is_some());
    }

    // Activation/Deactivation Tests

    #[tokio::test]
    async fn test_deactivate_api_key() {
        let (db, api_key_service, user) = setup_test_env().await;

        let api_key = create_test_api_key(&db.db, user.id, "Test Key", "admin").await;

        let response = api_key_service
            .deactivate_api_key(user.id, api_key.id)
            .await
            .unwrap();

        assert!(!response.is_active);
    }

    #[tokio::test]
    async fn test_activate_api_key() {
        let (db, api_key_service, user) = setup_test_env().await;

        // Create inactive key
        let unique_id = uuid::Uuid::new_v4();
        let api_key = api_keys::ActiveModel {
            name: Set("Inactive Key".to_string()),
            key_hash: Set(format!("test_hash_{}", unique_id)),
            key_prefix: Set(format!("tk_{}", &unique_id.to_string()[..8])),
            user_id: Set(user.id),
            role_type: Set("admin".to_string()),
            is_active: Set(false), // Start as inactive
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let api_key = api_key.insert(db.db.as_ref()).await.unwrap();

        let response = api_key_service
            .activate_api_key(user.id, api_key.id)
            .await
            .unwrap();

        assert!(response.is_active);
    }

    // Helper Method Tests

    #[tokio::test]
    async fn test_generate_api_key_format() {
        let (_db, api_key_service, _user) = setup_test_env().await;

        let key = api_key_service.generate_api_key();

        assert!(key.starts_with("tk_"));
        assert_eq!(key.len(), 43); // tk_ + 40 random chars

        // Check that it only contains valid characters
        let valid_chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        for c in key[3..].chars() {
            assert!(valid_chars.contains(c));
        }
    }

    #[tokio::test]
    async fn test_hash_api_key_consistency() {
        let (_db, api_key_service, _user) = setup_test_env().await;

        let api_key = "tk_testkey123456";

        let hash1 = api_key_service.hash_api_key(api_key);
        let hash2 = api_key_service.hash_api_key(api_key);

        // Same input should produce same hash
        assert_eq!(hash1, hash2);

        // Hash should be 64 chars (SHA256 hex)
        assert_eq!(hash1.len(), 64);
    }

    #[tokio::test]
    async fn test_hash_api_key_different_inputs() {
        let (_db, api_key_service, _user) = setup_test_env().await;

        let hash1 = api_key_service.hash_api_key("tk_key1");
        let hash2 = api_key_service.hash_api_key("tk_key2");

        // Different inputs should produce different hashes
        assert_ne!(hash1, hash2);
    }

    // Error Conversion Tests

    #[tokio::test]
    async fn test_error_to_problem_conversion() {
        let db_error =
            ApiKeyServiceError::DatabaseError(sea_orm::DbErr::RecordNotFound("test".to_string()));
        let problem = db_error.to_problem();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);

        let not_found = ApiKeyServiceError::NotFound("Key not found".to_string());
        let problem = not_found.to_problem();
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);

        let validation = ApiKeyServiceError::ValidationError("Invalid input".to_string());
        let problem = validation.to_problem();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);

        let unauthorized = ApiKeyServiceError::Unauthorized("Invalid key".to_string());
        let problem = unauthorized.to_problem();
        assert_eq!(problem.status_code, StatusCode::UNAUTHORIZED);

        let conflict = ApiKeyServiceError::Conflict("Name exists".to_string());
        let problem = conflict.to_problem();
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
    }
}
