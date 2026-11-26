// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::permissions::{Permission, Role};
use chrono::Utc;
use rand::RngExt;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
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
use tracing::warn;

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
    pub permissions: Option<Vec<String>>,
    pub is_active: bool,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub last_used_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub created_at: UtcDateTime,
    /// Immutable optional deployment resource scope.
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
}

impl From<temps_entities::api_keys::Model> for ApiKeyResponse {
    fn from(model: temps_entities::api_keys::Model) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            environment_id: model.environment_id,
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
    pub permissions: Option<Vec<String>>,
    pub api_key: String, // Only returned on creation
    #[schema(value_type = Option<String>, format = "date-time", example = "2024-12-31T23:59:59Z")]
    pub expires_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = "date-time", example = "2024-01-01T00:00:00Z")]
    pub created_at: UtcDateTime,
    /// Immutable optional deployment resource scope.
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
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
    /// Immutable optional deployment resource scope.
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
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

/// A validated key retains its immutable scope through authentication adapters.
pub struct ValidatedApiKey {
    pub user: users::Model,
    pub role: Option<Role>,
    pub permissions: Option<Vec<Permission>>,
    pub name: String,
    pub id: i32,
    pub scope: Option<temps_authz::ApiKeyScope>,
}

impl ValidatedApiKey {
    pub fn into_auth_context(self) -> crate::context::AuthContext {
        crate::context::AuthContext::new_api_key(
            self.user,
            self.role,
            self.permissions,
            self.name,
            self.id,
        )
        .with_api_key_scope(self.scope)
    }
}

pub struct ApiKeyService {
    db: Arc<DbConnection>,
    /// Throttles `last_used_at` writes so repeated validations of the same
    /// key within the window skip the DB write entirely. See
    /// [`crate::last_used_throttle`].
    last_used_throttle: crate::last_used_throttle::LastUsedThrottle,
}

impl ApiKeyService {
    pub fn new(db: Arc<DbConnection>) -> Self {
        Self {
            db,
            last_used_throttle: crate::last_used_throttle::LastUsedThrottle::new(
                crate::last_used_throttle::LAST_USED_UPDATE_INTERVAL,
            ),
        }
    }

    async fn validate_scope<C: sea_orm::ConnectionTrait>(
        connection: &C,
        key_name: &str,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        lock: bool,
    ) -> Result<(), ApiKeyServiceError> {
        let Some(project_id) = project_id else {
            if let Some(environment_id) = environment_id {
                return Err(ApiKeyServiceError::ValidationError(format!(
                    "API key '{key_name}' needs a project scope for environment {environment_id}"
                )));
            }
            return Ok(());
        };
        if project_id <= 0 || environment_id.is_some_and(|id| id <= 0) {
            return Err(ApiKeyServiceError::ValidationError(format!(
                "API key '{key_name}' requires positive project and environment IDs"
            )));
        }
        let query = temps_entities::projects::Entity::find_by_id(project_id)
            .filter(temps_entities::projects::Column::DeletedAt.is_null())
            .filter(temps_entities::projects::Column::IsDeleted.eq(false));
        let query = if lock { query.lock_shared() } else { query };
        if query.one(connection).await?.is_none() {
            return Err(ApiKeyServiceError::NotFound(format!(
                "Project {project_id} for API key '{key_name}' was not found"
            )));
        }
        if let Some(environment_id) = environment_id {
            let query = temps_entities::environments::Entity::find_by_id(environment_id)
                .filter(temps_entities::environments::Column::ProjectId.eq(project_id))
                .filter(temps_entities::environments::Column::DeletedAt.is_null());
            let query = if lock { query.lock_shared() } else { query };
            if query.one(connection).await?.is_none() {
                return Err(ApiKeyServiceError::NotFound(format!("Active environment {environment_id} in project {project_id} for API key '{key_name}' was not found")));
            }
        }
        Ok(())
    }

    pub async fn create_api_key(
        &self,
        user_id: i32,
        request: CreateApiKeyRequest,
    ) -> Result<CreateApiKeyResponse, ApiKeyServiceError> {
        // Validate role type and permissions
        let permissions_json = if request.role_type == "custom" {
            // For custom role, validate that permissions are provided
            let permissions = match request.permissions.as_ref() {
                Some(permissions) if !permissions.is_empty() => permissions,
                _ => {
                    return Err(ApiKeyServiceError::ValidationError(
                        "Custom role requires at least one permission".to_string(),
                    ));
                }
            };

            // Validate each permission
            for perm_str in permissions {
                if Permission::from_str(perm_str).is_none() {
                    return Err(ApiKeyServiceError::ValidationError(format!(
                        "Invalid permission: {}",
                        perm_str
                    )));
                }
            }

            // Store permissions as JSON string
            Some(serde_json::to_string(&permissions).map_err(|e| {
                ApiKeyServiceError::InternalServerError(format!(
                    "Failed to serialize permissions for API key '{}' of user {}: {}",
                    request.name, user_id, e
                ))
            })?)
        } else {
            // For predefined roles, validate the role exists
            if Role::from_str(&request.role_type).is_none() {
                return Err(ApiKeyServiceError::ValidationError(
                    format!("Invalid role type: {}. Valid roles are: admin, platform_admin, user, reader, api_reader, or custom", request.role_type)
                ));
            }
            None
        };

        if request.project_id.is_some()
            && request
                .expires_at
                .is_some_and(|expiry| expiry <= Utc::now())
        {
            return Err(ApiKeyServiceError::ValidationError(format!(
                "Expiry for API key '{}' must be in the future",
                request.name
            )));
        }
        temps_authz::ApiKeyScope::from_ids(request.project_id, request.environment_id).map_err(
            |error| {
                ApiKeyServiceError::ValidationError(format!("API key '{}': {error}", request.name))
            },
        )?;
        let txn = self.db.begin().await?;
        Self::validate_scope(
            &txn,
            &request.name,
            request.project_id,
            request.environment_id,
            true,
        )
        .await?;
        // Check if name is unique for this user
        let existing_key = ApiKeyEntity::find()
            .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
            .filter(temps_entities::api_keys::Column::Name.eq(&request.name))
            .one(&txn)
            .await?;

        if existing_key.is_some() {
            return Err(ApiKeyServiceError::Conflict(
                "API key with this name already exists".to_string(),
            ));
        }

        // Generate API key
        let api_key = self.generate_api_key_for_scope(request.project_id);
        let key_hash = self.hash_api_key(&api_key);
        let key_prefix = api_key.chars().take(8).collect::<String>();

        let now = Utc::now();
        let expires_at = request.expires_at.or_else(|| {
            // Default expiration: 1 year from now
            Some(now + chrono::Duration::days(365))
        });

        let new_api_key = ApiKeyActiveModel {
            project_id: Set(request.project_id),
            environment_id: Set(request.environment_id),
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

        let api_key_model = new_api_key.insert(&txn).await?;
        txn.commit().await?;

        Ok(CreateApiKeyResponse {
            id: api_key_model.id,
            project_id: api_key_model.project_id,
            environment_id: api_key_model.environment_id,
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
        let api_keys_models = paginator
            .fetch_page(temps_core::config::page_index(page))
            .await?;

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

    /// Delete one of `user_id`'s keys, returning the deleted key's
    /// (secret-free) description so the caller can audit what was removed.
    pub async fn delete_api_key(
        &self,
        user_id: i32,
        api_key_id: i32,
    ) -> Result<ApiKeyResponse, ApiKeyServiceError> {
        let api_key = ApiKeyEntity::find_by_id(api_key_id)
            .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                ApiKeyServiceError::NotFound(format!(
                    "API key {api_key_id} not found for user {user_id}"
                ))
            })?;

        ApiKeyEntity::delete_by_id(api_key.id)
            .exec(self.db.as_ref())
            .await?;

        Ok(ApiKeyResponse::from(api_key))
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
        let validated = self.validate_api_key_with_scope(api_key).await?;
        if validated.scope.is_some() {
            return Err(ApiKeyServiceError::Unauthorized(
                "Scoped API keys require a scope-aware authentication adapter".into(),
            ));
        }
        Ok((
            validated.user,
            validated.role,
            validated.permissions,
            validated.name,
            validated.id,
        ))
    }

    pub async fn validate_api_key_with_scope(
        &self,
        api_key: &str,
    ) -> Result<ValidatedApiKey, ApiKeyServiceError> {
        let key_hash = self.hash_api_key(api_key);
        let key_prefix = api_key.chars().take(8).collect::<String>();

        let api_key_model = ApiKeyEntity::find()
            .filter(temps_entities::api_keys::Column::KeyHash.eq(&key_hash))
            .filter(temps_entities::api_keys::Column::KeyPrefix.eq(&key_prefix))
            .filter(temps_entities::api_keys::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApiKeyServiceError::Unauthorized("Invalid API key".to_string()))?;

        let scope = temps_authz::ApiKeyScope::from_ids(
            api_key_model.project_id,
            api_key_model.environment_id,
        )
        .map_err(|error| {
            ApiKeyServiceError::Unauthorized(format!("API key {}: {error}", api_key_model.id))
        })?;
        if api_key.starts_with("tkp_") != scope.is_some() {
            return Err(ApiKeyServiceError::Unauthorized(format!(
                "API key {} has an incompatible scope prefix",
                api_key_model.id
            )));
        }
        Self::validate_scope(
            self.db.as_ref(),
            &api_key_model.name,
            api_key_model.project_id,
            api_key_model.environment_id,
            false,
        )
        .await?;

        // Check if expired
        if let Some(expires_at) = api_key_model.expires_at {
            if expires_at <= Utc::now() {
                return Err(ApiKeyServiceError::Unauthorized(
                    "API key has expired".to_string(),
                ));
            }
        }

        // Get user, excluding soft-deleted users
        let user = users::Entity::find_by_id(api_key_model.user_id)
            .filter(users::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| ApiKeyServiceError::NotFound("User not found or deleted".to_string()))?;

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

        // Bound the stored grant by what the owner holds now, not what they
        // held when the key was minted.
        let owner_role = current_owner_role(self.db.as_ref(), user.id).await?;
        let (role, permissions) = cap_api_key_grant(role, permissions, &owner_role);

        self.record_last_used(&api_key_model).await;

        Ok(ValidatedApiKey {
            user,
            role,
            permissions,
            name: api_key_model.name,
            id: api_key_model.id,
            scope,
        })
    }

    /// Record that `api_key` just authenticated a request.
    ///
    /// Writes are throttled per key id so repeated validations within
    /// `LAST_USED_UPDATE_INTERVAL` skip the DB write, except a key's first
    /// use (`last_used_at` still empty), which is always written: the AI
    /// harness onboarding waits on exactly that timestamp, and a throttle
    /// primed under the same id (by the previous secret of a rotated key)
    /// must not hide it. The write is conditioned on the validated secret's
    /// hash. A failed write never fails the request; it is logged and the
    /// throttle entry dropped, so the next request retries.
    async fn record_last_used(&self, api_key: &temps_entities::api_keys::Model) {
        let first_use = api_key.last_used_at.is_none();
        // Always consult the throttle so a first use also starts the window.
        let throttle_allows = self.last_used_throttle.should_update(api_key.id);
        if !(first_use || throttle_allows) {
            return;
        }
        // Bound to the secret that was validated, not just the row: if the key
        // was rotated after validation read it, this matches no row, so the
        // replacement secret is never stamped as used by a request that sent
        // the old one. Zero rows affected is not an error for the request.
        let result = ApiKeyEntity::update_many()
            .col_expr(
                temps_entities::api_keys::Column::LastUsedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .filter(temps_entities::api_keys::Column::Id.eq(api_key.id))
            .filter(temps_entities::api_keys::Column::KeyHash.eq(&api_key.key_hash))
            .exec(self.db.as_ref())
            .await;
        if let Err(error) = result {
            self.last_used_throttle.forget(api_key.id);
            warn!(
                api_key_id = api_key.id,
                user_id = api_key.user_id,
                key_prefix = %api_key.key_prefix,
                first_use,
                error = %error,
                "Failed to record last_used_at for API key {}; the request stays authenticated and the next one retries the write",
                api_key.id
            );
        }
    }

    /// The caller's AI harness connection status.
    ///
    /// Two indexed lookups on the caller's own keys: the dedicated
    /// [`AI_HARNESS_KEY_NAME`](crate::ai_harness::AI_HARNESS_KEY_NAME) key
    /// (names are unique per user), and, only while that key is unused, the
    /// most recently used device-login key created after it. Unlike the
    /// paginated key list it never misses the dedicated key behind newer ones.
    pub async fn ai_harness_status(
        &self,
        user_id: i32,
    ) -> Result<crate::ai_harness::AiHarnessStatusResponse, ApiKeyServiceError> {
        use temps_entities::api_keys::Column;

        let harness_key = ApiKeyEntity::find()
            .filter(Column::UserId.eq(user_id))
            .filter(Column::Name.eq(crate::ai_harness::AI_HARNESS_KEY_NAME))
            .one(self.db.as_ref())
            .await?;

        let cli_login_key = match &harness_key {
            Some(key) if key.is_active && key.last_used_at.is_none() => {
                ApiKeyEntity::find()
                    .filter(Column::UserId.eq(user_id))
                    .filter(Column::IsActive.eq(true))
                    .filter(Column::ProjectId.is_null())
                    .filter(Column::Name.starts_with(crate::ai_harness::CLI_DEVICE_KEY_PREFIX))
                    .filter(Column::CreatedAt.gte(key.created_at))
                    .filter(Column::LastUsedAt.is_not_null())
                    .order_by_desc(Column::LastUsedAt)
                    .one(self.db.as_ref())
                    .await?
            }
            _ => None,
        };

        Ok(crate::ai_harness::derive_ai_harness_status(
            harness_key.as_ref(),
            cli_login_key.as_ref(),
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

    /// Rotate an API key's secret in place.
    ///
    /// Generates a brand new plaintext secret for the same key record (same
    /// id, name, role/permissions, and expiry policy), overwrites the stored
    /// hash so the old secret stops authenticating immediately, and returns
    /// the new plaintext secret exactly once -- mirroring `create_api_key`'s
    /// "only shown at creation/rotation time" contract.
    pub async fn rotate_api_key(
        &self,
        user_id: i32,
        api_key_id: i32,
    ) -> Result<CreateApiKeyResponse, ApiKeyServiceError> {
        let api_key = ApiKeyEntity::find_by_id(api_key_id)
            .filter(temps_entities::api_keys::Column::UserId.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                ApiKeyServiceError::NotFound(format!("API key {} not found", api_key_id))
            })?;

        let new_secret = self.generate_api_key_for_scope(api_key.project_id);
        let key_hash = self.hash_api_key(&new_secret);
        let key_prefix = new_secret.chars().take(8).collect::<String>();

        let mut api_key_active: ApiKeyActiveModel = api_key.into();
        api_key_active.key_hash = Set(key_hash);
        api_key_active.key_prefix = Set(key_prefix.clone());
        // The old secret is gone, so any "last used" timestamp recorded
        // against it no longer means anything for the new secret.
        api_key_active.last_used_at = Set(None);
        api_key_active.updated_at = Set(Utc::now());

        let updated = api_key_active.update(self.db.as_ref()).await?;

        Ok(CreateApiKeyResponse {
            id: updated.id,
            project_id: updated.project_id,
            environment_id: updated.environment_id,
            name: updated.name,
            key_prefix,
            role_type: updated.role_type,
            permissions: updated
                .permissions
                .and_then(|p| serde_json::from_str(&p).ok()),
            api_key: new_secret, // Only returned on rotation
            expires_at: updated.expires_at,
            created_at: updated.created_at,
        })
    }

    /// Generate a service-scoped metrics ingest key (`si_` prefix).
    ///
    /// The key is tied to the given `service_id` and uses the
    /// `metrics_ingest` role.  It never expires — it lives until the
    /// external service is deleted (cascade via FK).
    ///
    /// Returns the plaintext key (caller must inject into the container;
    /// it is never stored in plaintext).
    pub async fn create_service_ingest_key(
        &self,
        service_id: i32,
        service_name: &str,
        system_user_id: i32,
    ) -> Result<String, ApiKeyServiceError> {
        let random_part: String = rand::rng()
            .sample_iter(&rand::distr::Alphanumeric)
            .take(40)
            .map(char::from)
            .collect();
        let api_key = format!("si_{}", random_part);

        let key_hash = self.hash_api_key(&api_key);
        let key_prefix = api_key.chars().take(8).collect::<String>();

        temps_entities::api_keys::ActiveModel {
            name: Set(format!("metrics-ingest-{}", service_name)),
            key_hash: Set(key_hash),
            key_prefix: Set(key_prefix),
            user_id: Set(system_user_id),
            role_type: Set("metrics_ingest".to_string()),
            is_active: Set(true),
            service_id: Set(Some(service_id)),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .map_err(ApiKeyServiceError::DatabaseError)?;

        Ok(api_key)
    }

    /// Revoke all metrics ingest keys for a given service.
    /// Called when the service is being deleted (belt-and-suspenders alongside CASCADE).
    pub async fn revoke_service_ingest_keys(
        &self,
        service_id: i32,
    ) -> Result<(), ApiKeyServiceError> {
        temps_entities::api_keys::Entity::delete_many()
            .filter(temps_entities::api_keys::Column::ServiceId.eq(service_id))
            .exec(self.db.as_ref())
            .await
            .map_err(ApiKeyServiceError::DatabaseError)?;
        Ok(())
    }

    fn generate_api_key_for_scope(&self, project_id: Option<i32>) -> String {
        let secret = self.generate_api_key();
        if project_id.is_some() {
            // Older servers refuse this prefix instead of dropping the scope.
            secret.replacen("tk_", "tkp_", 1)
        } else {
            secret
        }
    }

    fn generate_api_key(&self) -> String {
        const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let mut rng = rand::rng();

        let prefix = "tk_";
        let random_part: String = (0..40)
            .map(|_| {
                let idx = rng.random_range(0..CHARSET.len());
                CHARSET[idx] as char
            })
            .collect();

        format!("{}{}", prefix, random_part)
    }

    pub(crate) fn hash_api_key(&self, api_key: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(api_key.as_bytes());
        hex::encode(hasher.finalize())
    }
}

/// The role a user holds *right now*, resolved from `user_roles` on every
/// call and never cached on a credential.
///
/// Delegates to `user_service::current_session_role`, the mapping the
/// session middleware applies to browser sessions, so an API key is bounded by
/// exactly what its owner would get by signing in. Public so crates that
/// re-resolve a key outside this service (plugin host bridge, AI tool
/// refresh) apply the same ceiling.
pub async fn current_owner_role<C: sea_orm::ConnectionTrait>(
    db: &C,
    user_id: i32,
) -> Result<Role, sea_orm::DbErr> {
    crate::user_service::current_session_role(db, user_id).await
}

/// Narrow an API key's stored grant to what its owner's current role still
/// grants, so a key can never exceed the user who owns it.
///
/// The role/permission set is copied onto the key row when it is minted and
/// is not rewritten when the owner is later demoted. Without this cap an
/// `admin` key (including the one `temps login` mints) keeps admin access
/// for its whole lifetime after `DELETE /users/{id}/roles/admin`.
///
/// - Predefined role whose permissions are all still held by the owner: the
///   role is kept as-is, so role checks such as `is_admin()` keep working.
/// - Predefined role that now exceeds the owner: the key degrades to a
///   custom grant of the intersection. `effective_role` becomes `Custom`, so
///   role-based checks (`is_admin`, `is_instance_admin`) no longer pass.
/// - Custom permission list: filtered to the permissions the owner holds.
///
/// Service ingest keys (`metrics_ingest`) carry an empty permission set, so
/// they are always a subset of any owner and pass through unchanged; they
/// authenticate only on the OTel `si_` path, which is bound to a service
/// row, not to user permissions. Deployment tokens (`dt_`) are not owned by
/// a user and never reach this function.
pub fn cap_api_key_grant(
    role: Option<Role>,
    permissions: Option<Vec<Permission>>,
    owner_role: &Role,
) -> (Option<Role>, Option<Vec<Permission>>) {
    let owner_permissions = owner_role.permissions();
    match role {
        Some(role) => {
            let role_permissions = role.permissions();
            if role_permissions
                .iter()
                .all(|permission| owner_permissions.contains(permission))
            {
                (Some(role), permissions)
            } else {
                let capped = role_permissions
                    .iter()
                    .filter(|permission| owner_permissions.contains(permission))
                    .copied()
                    .collect();
                (None, Some(capped))
            }
        }
        None => {
            let capped = permissions
                .unwrap_or_default()
                .into_iter()
                .filter(|permission| owner_permissions.contains(permission))
                .collect();
            (None, Some(capped))
        }
    }
}

#[cfg(test)]
mod tests;
